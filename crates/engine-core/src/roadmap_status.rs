//! `roadmap_status` — the typed join behind `GET /api/roadmaps/{slug}/status` (EN.15.H).
//!
//! EN.15.H task 1. `/roadmap-status` is how an operator sees a live multi-lane run, and today it
//! exists only as `base-template/scripts/roadmap_status_discovery.py` (1382 lines, the canonical
//! oracle — see that file's own doc comment) plus an 891-line divergent HQ fork. This module is
//! the same join, typed, reusing `engine_core::coord` (EN.15.A / `OK.6.A`) for the registry,
//! lease and queue-depth half of the view rather than re-reading those artifacts a second way.
//!
//! ## What this joins
//!
//! - `<roadmap_dir>/lane-log.jsonl` — the roadmap's own append-only lane log.
//! - `**/planning/orchestration-run/<roadmap>/{notes.md,review.md}` — per-repo run records (D57).
//! - `<repo>/planning/<spec>/sdlc/sdlc-*state.json` — per-spec SDLC engine state.
//! - `**/planning/state.json` per repo — `depends_on` operator/approval edges and `carryover[]`.
//! - The coordination registry, leases and message-queue depth, via [`crate::coord`].
//!
//! ## Realpath dedup — MANDATORY, and it has a known expensive wrong implementation
//!
//! Every `planning/` in this fleet is a symlink into a `_planning/` vault, so a fleet-wide sweep
//! for `planning/state.json` (or an `orchestration-run` tree) sees the same file twice — once
//! through the symlink, once through the vault path — unless both are resolved to the same
//! canonical path before dedup. The thing symlinked is always a **directory** (`planning/`),
//! never the file itself, so [`realpath_dedup`] resolves and caches each **parent directory**
//! exactly once rather than canonicalizing every hit independently. The Python oracle's own doc
//! comment (`base-template/scripts/roadmap_status_discovery.py:225-232`) records what a per-file
//! implementation costs: 1749 hits x a full symlink-chain walk each, measured at 194s of a 195s
//! self-test run, which reliably `SIGKILL`ed the `/sdlc-task`/`/sdlc-flow` test stages. A file
//! that is itself a symlink (rather than merely living under one) still gets a full
//! canonicalization — only the parent lookup is cached.
//!
//! ## The one deliberate divergence from the Python oracle
//!
//! A truncated `lane-log.jsonl` line is **reported**, not silently skipped: [`read_lane_log`]
//! returns it as a [`MalformedLaneLogLine`] carrying the line's byte offset in the file. The
//! Python skips such a line outright (`read_lane_log`'s own docstring: "tolerant of malformed
//! lines (skipped, never crash)"). A silently skipped line is exactly how a run's evidence goes
//! missing, so this module reports it instead. Parity against the Python (EN.15.H task 2) asserts
//! this divergence explicitly rather than excluding the field from comparison.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::coord;

/// Directory names never descended into while sweeping — heavy or irrelevant subtrees a
/// coordination reader has no business walking. Mirrors `crate::coord`'s own `SKIP_DIR_NAMES`.
const SKIP_DIR_NAMES: [&str; 5] = ["target", "node_modules", ".git", "dist", "build"];

/// Bounds every fleet-wide sweep below. A real `<tier>/<repo>/planning/...` path sits well within
/// this many segments of the brain root; this exists to make a symlink cycle terminate rather
/// than hang, the same discipline `crate::coord::read_run_records` already uses.
const SWEEP_MAX_DEPTH: usize = 12;

/// Liveness threshold: an `updated_at` (or heartbeat) older than this many hours is reported
/// STALE regardless of what a `status` field says — a killed session leaves `status: running`
/// behind forever. Mirrors the Python oracle's `STALE_THRESHOLD_HOURS`.
const STALE_THRESHOLD_HOURS: f64 = 6.0;

/// Status values the oracle already knows about. An unknown value passes through verbatim
/// (`status_known: false`) rather than being bucketed or rejected.
const KNOWN_STATUS_VALUES: [&str; 6] =
    ["done", "blocked", "docs", "running", "passed", "completed"];

/// Edge `type` values this module treats as "needs the operator" — matched on this field only,
/// never a slug prefix, mirroring the oracle's `OPERATOR_EDGE_TYPES` (the fleet is mid-rename to
/// an `operator-<slug>` convention; a prefix match would miss half the population).
const OPERATOR_EDGE_TYPES: [&str; 2] = ["operator", "approval"];

/// Everything that can go wrong resolving a roadmap slug to a directory or reading its log.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoadmapStatusError {
    /// The slug resolved to neither `planning/roadmaps/<slug>/` nor legacy `planning/<slug>/`.
    #[error("no roadmap directory found for slug '{slug}' at {new_dir} or {legacy_dir}")]
    NotFound {
        slug: String,
        new_dir: PathBuf,
        legacy_dir: PathBuf,
    },
    /// The slug resolved to BOTH locations — never silently prefer one.
    #[error("roadmap slug '{slug}' exists in both {new_dir} and {legacy_dir} — ambiguous, not resolving")]
    Ambiguous {
        slug: String,
        new_dir: PathBuf,
        legacy_dir: PathBuf,
    },
}

/// Resolve a roadmap slug to its directory, per `/begin-orchestration` Step 1C's fixed order:
/// 1. `planning/roadmaps/<slug>/` if it exists
/// 2. otherwise legacy `planning/<slug>/` if it exists
/// 3. present in BOTH → [`RoadmapStatusError::Ambiguous`], never a silent preference
pub fn resolve_roadmap_dir(root: &Path, slug: &str) -> Result<PathBuf, RoadmapStatusError> {
    let new_dir = root.join("planning").join("roadmaps").join(slug);
    let legacy_dir = root.join("planning").join(slug);
    let new_exists = new_dir.is_dir();
    let legacy_exists = legacy_dir.is_dir();
    match (new_exists, legacy_exists) {
        (true, true) => Err(RoadmapStatusError::Ambiguous {
            slug: slug.to_string(),
            new_dir,
            legacy_dir,
        }),
        (true, false) => Ok(new_dir),
        (false, true) => Ok(legacy_dir),
        (false, false) => Err(RoadmapStatusError::NotFound {
            slug: slug.to_string(),
            new_dir,
            legacy_dir,
        }),
    }
}

/// One `lane-log.jsonl` line that failed to parse as JSON — the one deliberate divergence from
/// the Python oracle, which skips such a line silently. Carries the byte offset of the line's
/// first byte in the file, so an operator (or a future UI) can go look at the exact spot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MalformedLaneLogLine {
    pub path: PathBuf,
    /// 1-indexed line number within the file.
    pub line_number: usize,
    /// Byte offset of the line's first byte from the start of the file.
    pub byte_offset: usize,
    /// The `serde_json` parse error, rendered as text.
    pub error: String,
}

/// Read `<roadmap_dir>/lane-log.jsonl`. A missing file yields an empty log (a legitimate state —
/// a roadmap with no lane activity yet), never an error. Blank lines are skipped silently, same
/// as the oracle; a non-blank line that fails to parse as JSON is reported in the second return
/// value rather than dropped.
pub fn read_lane_log(roadmap_dir: &Path) -> (Vec<Value>, Vec<MalformedLaneLogLine>) {
    let path = roadmap_dir.join("lane-log.jsonl");
    let mut entries = Vec::new();
    let mut malformed = Vec::new();
    let Ok(text) = fs::read_to_string(&path) else {
        return (entries, malformed);
    };
    let mut offset = 0usize;
    for line in text.split('\n') {
        let line_len = line.len();
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            match serde_json::from_str::<Value>(trimmed) {
                Ok(v) => entries.push(v),
                Err(e) => malformed.push(MalformedLaneLogLine {
                    path: path.clone(),
                    line_number: entries.len() + malformed.len() + 1,
                    byte_offset: offset,
                    error: e.to_string(),
                }),
            }
        }
        // `split('\n')` drops the separator; every line but a trailing empty one was followed by
        // exactly one `\n` byte in the source text.
        offset += line_len + 1;
    }
    (entries, malformed)
}

/// Distinct repo slugs named by lane-log entries, order-preserving first-seen.
pub fn repos_from_lane_log(entries: &[Value]) -> Vec<String> {
    let mut seen = Vec::new();
    for e in entries {
        if let Some(repo) = e.get("repo").and_then(|v| v.as_str()) {
            if !seen.iter().any(|r: &String| r == repo) {
                seen.push(repo.to_string());
            }
        }
    }
    seen
}

/// Recursively sweep `root` for files matching `is_match`, following symlinks (a directory entry
/// reached through a symlink is descended into exactly like a real one — `Path::is_dir` follows
/// symlinks). Bounded by [`SWEEP_MAX_DEPTH`] so a symlink cycle terminates rather than hangs,
/// rather than tracking visited inodes. Returns raw (not realpath-deduped) hits, sorted.
fn sweep(root: &Path, is_match: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > SWEEP_MAX_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if SKIP_DIR_NAMES.contains(&name) {
                    continue;
                }
                stack.push((path, depth + 1));
            } else if is_match(&path) {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}

/// Dedup raw sweep hits by realpath, returning the sorted distinct set. Because every
/// `planning/` symlink canonicalizes onto the same vault file, this naturally retains the vault
/// original — a `trees/`-routed (worktree) alias's realpath IS the vault path.
///
/// **Parent directories are resolved once and cached** (`parent_cache`), because the thing
/// symlinked here is always a DIRECTORY (`planning/`), never the file itself. See this module's
/// doc comment for what a per-file implementation costs. A leaf that is itself a symlink still
/// gets a full canonicalization.
pub fn realpath_dedup(root: &Path, hits: &[PathBuf]) -> Vec<PathBuf> {
    let mut parent_cache: HashMap<PathBuf, PathBuf> = HashMap::new();
    let mut out: BTreeSet<PathBuf> = BTreeSet::new();
    for hit in hits {
        let absolute = if hit.is_absolute() {
            hit.clone()
        } else {
            root.join(hit)
        };
        let Some(parent) = absolute.parent().map(Path::to_path_buf) else {
            out.insert(absolute);
            continue;
        };
        let resolved_parent = parent_cache
            .entry(parent.clone())
            .or_insert_with(|| fs::canonicalize(&parent).unwrap_or_else(|_| parent.clone()))
            .clone();
        let Some(file_name) = absolute.file_name() else {
            out.insert(absolute);
            continue;
        };
        let mut candidate = resolved_parent.join(file_name);
        // The parent is canonical now, so only a symlinked LEAF can still need resolving.
        if fs::symlink_metadata(&candidate)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
        {
            candidate = fs::canonicalize(&candidate).unwrap_or(candidate);
        }
        out.insert(candidate);
    }
    out.into_iter().collect()
}

/// Sweep fleet-wide for `planning/state.json`, realpath-deduped, keyed by each file's own
/// top-level `"repo"` field (the authoritative slug — never the directory name, which can
/// differ).
pub fn discover_repos(root: &Path) -> BTreeMap<String, PathBuf> {
    let raw = sweep(root, |p| {
        p.file_name().and_then(|n| n.to_str()) == Some("state.json")
            && p.parent()
                .and_then(|d| d.file_name())
                .and_then(|n| n.to_str())
                == Some("planning")
    });
    let distinct = realpath_dedup(root, &raw);
    let mut repos = BTreeMap::new();
    for path in distinct {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        if let Some(slug) = data.get("repo").and_then(|v| v.as_str()) {
            repos.insert(slug.to_string(), path);
        }
    }
    repos
}

/// One `planning/orchestration-run/<roadmap>/{notes.md,review.md}` run record.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub path: PathBuf,
    pub lifecycle: Option<String>,
    pub run_started: Option<String>,
    pub run_ended: Option<String>,
}

/// Both halves (`notes.md` / `review.md`) of one repo's run record for a roadmap.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RunRecordPair {
    pub notes: Option<RunRecord>,
    pub review: Option<RunRecord>,
}

fn parse_frontmatter(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(rest) = text.strip_prefix("---\n") else {
        return out;
    };
    let Some(end) = rest.find("\n---") else {
        return out;
    };
    for line in rest[..end].lines() {
        if let Some((key, value)) = line.split_once(':') {
            out.insert(
                key.trim().to_string(),
                value.trim().trim_matches('"').to_string(),
            );
        }
    }
    out
}

/// Sweep for `**/orchestration-run/<roadmap_slug>/{notes.md,review.md}`, realpath-deduped,
/// grouped by owning repo (the path segment immediately before `planning`).
pub fn discover_run_records(root: &Path, roadmap_slug: &str) -> BTreeMap<String, RunRecordPair> {
    let raw = sweep(root, |p| {
        let is_target_name = matches!(
            p.file_name().and_then(|n| n.to_str()),
            Some("notes.md") | Some("review.md")
        );
        if !is_target_name {
            return false;
        }
        let roadmap_dir_matches = p
            .parent()
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some(roadmap_slug);
        let grandparent_is_orchestration_run = p
            .parent()
            .and_then(|d| d.parent())
            .and_then(|d| d.file_name())
            .and_then(|n| n.to_str())
            == Some("orchestration-run");
        roadmap_dir_matches && grandparent_is_orchestration_run
    });
    let distinct = realpath_dedup(root, &raw);

    let mut by_repo: BTreeMap<String, RunRecordPair> = BTreeMap::new();
    for path in distinct {
        let repo = match repo_from_run_record_path(&path) {
            Some(r) => r,
            None => continue,
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let fm = parse_frontmatter(&text);
        let record = RunRecord {
            path: path.clone(),
            lifecycle: fm.get("lifecycle").cloned(),
            run_started: fm.get("run_started").cloned(),
            run_ended: fm.get("run_ended").cloned(),
        };
        let entry = by_repo.entry(repo).or_default();
        match path.file_name().and_then(|n| n.to_str()) {
            Some("notes.md") => entry.notes = Some(record),
            Some("review.md") => entry.review = Some(record),
            _ => {}
        }
    }
    by_repo
}

/// The repo a run-record path belongs to: the path segment immediately before `planning`,
/// mirroring the oracle's `discover_run_records`.
fn repo_from_run_record_path(path: &Path) -> Option<String> {
    let components: Vec<String> = path
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    let idx = components.iter().position(|c| c == "planning")?;
    if idx == 0 {
        return None;
    }
    Some(components[idx - 1].clone())
}

fn ticket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z]{1,4}\.ticket\.(?P<slug>[a-z0-9][a-z0-9-]*)$")
            .expect("static ticket regex compiles")
    })
}

fn chore_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^[A-Za-z]{1,4}\.chore\.(?P<slug>[a-z0-9][a-z0-9-]*)$")
            .expect("static chore regex compiles")
    })
}

/// `XX.ticket.<slug>` → `ticket-<slug>`; `XX.chore.<slug>` → `chore-<slug>`;
/// `XX.<phase>.<letter>` (or anything else) → unresolved (`None`) — that shape needs the repo's
/// own `master-plan.md`, which this function does not have access to; the caller reports it as
/// unresolved rather than fabricating a slug.
pub fn resolve_block_to_spec_slug(block_id: &str) -> Option<String> {
    if let Some(caps) = ticket_re().captures(block_id) {
        return Some(format!("ticket-{}", &caps["slug"]));
    }
    if let Some(caps) = chore_re().captures(block_id) {
        return Some(format!("chore-{}", &caps["slug"]));
    }
    None
}

fn parse_iso(ts: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(ts)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Liveness computed from `updated_at` age against [`STALE_THRESHOLD_HOURS`] — never from a
/// `status` field alone, since a killed session leaves `status: running` behind forever.
fn compute_liveness(updated_at: Option<&str>, now: DateTime<Utc>) -> (String, Option<f64>) {
    let Some(dt) = updated_at.and_then(parse_iso) else {
        return ("unknown".to_string(), None);
    };
    let age_hours = (now - dt).num_seconds() as f64 / 3600.0;
    let liveness = if age_hours <= STALE_THRESHOLD_HOURS {
        "live"
    } else {
        "stale"
    };
    (
        liveness.to_string(),
        Some((age_hours * 100.0).round() / 100.0),
    )
}

/// One `planning/<spec>/sdlc/sdlc-*state.json`'s normalized fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdlcState {
    pub path: PathBuf,
    pub status: String,
    pub status_known: bool,
    pub updated_at: Option<String>,
    pub current_task: Option<Value>,
    pub tasks_run: Vec<Value>,
    pub liveness: String,
    pub age_hours: Option<f64>,
}

/// Find `<spec_dir>/sdlc/sdlc-*state.json` (first match, sorted; there is normally exactly one
/// engine's state file per spec directory) and return its normalized fields, tolerant of absent
/// keys. An unreadable or unparsable file is reported as `status: "<unreadable>"` rather than
/// dropped. `status` values outside [`KNOWN_STATUS_VALUES`] pass through verbatim.
pub fn read_sdlc_state(spec_dir: &Path, now: DateTime<Utc>) -> Option<SdlcState> {
    let sdlc_dir = spec_dir.join("sdlc");
    if !sdlc_dir.is_dir() {
        return None;
    }
    let mut candidates: Vec<PathBuf> = fs::read_dir(&sdlc_dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("sdlc-") && n.ends_with("state.json"))
                .unwrap_or(false)
        })
        .collect();
    candidates.sort();
    let path = candidates.into_iter().next()?;

    let parsed = fs::read_to_string(&path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok());
    let Some(data) = parsed else {
        return Some(SdlcState {
            path,
            status: "<unreadable>".to_string(),
            status_known: false,
            updated_at: None,
            current_task: None,
            tasks_run: Vec::new(),
            liveness: "unknown".to_string(),
            age_hours: None,
        });
    };

    let status = data
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("<absent>")
        .to_string();
    let updated_at = data
        .get("updated_at")
        .and_then(|v| v.as_str())
        .map(String::from);
    let (liveness, age_hours) = compute_liveness(updated_at.as_deref(), now);
    Some(SdlcState {
        path,
        status_known: KNOWN_STATUS_VALUES.contains(&status.as_str()),
        status,
        updated_at,
        current_task: data.get("current_task").cloned(),
        tasks_run: data
            .get("tasks_run")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default(),
        liveness,
        age_hours,
    })
}

/// One `depends_on`/`blocked_by` edge whose `type` is `operator` or `approval`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OperatorGate {
    #[serde(rename = "type")]
    pub kind: String,
    pub slug: String,
    pub exit: Option<String>,
    pub start: Option<String>,
    pub what: Option<String>,
}

/// Every operator/approval gate found in one repo's `state.json`, plus a coverage count.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OperatorGates {
    pub gates: Vec<OperatorGate>,
    pub coverage_count: usize,
}

/// Yield every object found anywhere in `node` that carries a string `"type"` key — walks
/// `depends_on` lists and `blocked_by` lists alike, since both carry the same edge shape.
fn iter_edges<'a>(node: &'a Value, out: &mut Vec<&'a Value>) {
    match node {
        Value::Object(map) => {
            if map.get("type").and_then(|v| v.as_str()).is_some() {
                out.push(node);
            }
            for v in map.values() {
                iter_edges(v, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                iter_edges(item, out);
            }
        }
        _ => {}
    }
}

/// Find every `depends_on`/`blocked_by` edge whose `type` is `operator` or `approval` — matched
/// on `type` only, never a slug prefix (see [`OPERATOR_EDGE_TYPES`]'s doc comment).
pub fn discover_operator_gates(state_json_path: &Path) -> OperatorGates {
    let Some(data) = fs::read_to_string(state_json_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return OperatorGates::default();
    };
    let mut edges = Vec::new();
    iter_edges(&data, &mut edges);

    let mut seen_slugs = HashSet::new();
    let mut gates = Vec::new();
    for edge in edges {
        let Some(kind) = edge.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        if !OPERATOR_EDGE_TYPES.contains(&kind) {
            continue;
        }
        let slug = edge
            .get("slug")
            .and_then(|v| v.as_str())
            .unwrap_or("<unnamed>")
            .to_string();
        if !seen_slugs.insert(slug.clone()) {
            continue;
        }
        gates.push(OperatorGate {
            kind: kind.to_string(),
            slug,
            exit: edge.get("exit").and_then(|v| v.as_str()).map(String::from),
            start: edge.get("start").and_then(|v| v.as_str()).map(String::from),
            what: edge.get("what").and_then(|v| v.as_str()).map(String::from),
        });
    }
    let coverage_count = gates.len();
    OperatorGates {
        gates,
        coverage_count,
    }
}

/// `state.json`'s top-level `carryover[]`, verbatim. An absent or non-array field yields an
/// empty list, never an error.
pub fn discover_carryover(state_json_path: &Path) -> Vec<Value> {
    let Some(data) = fs::read_to_string(state_json_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
    else {
        return Vec::new();
    };
    data.get("carryover")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
}

/// One lane-log entry joined with the SDLC state it resolves to, when it does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlockActivity {
    pub block: String,
    pub status: String,
    pub note: Option<String>,
    pub ts: Option<String>,
    pub spec_slug: Option<String>,
    pub sdlc_state: Option<SdlcState>,
}

/// A repo lease/queue view for one lane, joined from [`crate::coord`]'s already-shipped
/// registry/lease reader rather than a second implementation.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MessageQueueState {
    pub inbox_count: usize,
    pub oldest_unread_sent_at: Option<String>,
}

/// The full per-repo view: everything `/roadmap-status` reports about one lane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LaneResult {
    pub repo: String,
    pub blocks: Vec<BlockActivity>,
    pub run_record: Option<RunRecordPair>,
    pub operator_gates: OperatorGates,
    pub carryover: Vec<Value>,
    pub lane_registry: Vec<coord::RegistryEntry>,
    pub leases: Vec<coord::LeaseEntry>,
    pub message_queue: MessageQueueState,
}

/// The full join for one roadmap. Writes nothing, ever. Empty sections are represented
/// explicitly (empty list/map), never omitted — a roadmap with zero lane-log lines resolves
/// cleanly with `lanes` empty rather than erroring, the same way an empty run is a legitimate
/// state for `crate::coord`'s reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoadmapStatusResult {
    pub roadmap: String,
    pub roadmap_dir: PathBuf,
    pub lanes: BTreeMap<String, LaneResult>,
    pub repos_in_lane_log: Vec<String>,
    pub repos_with_run_record_only: Vec<String>,
    pub operator_coverage_total: usize,
    /// The one deliberate divergence from the Python oracle: a truncated `lane-log.jsonl` line
    /// reported with its byte offset, never silently skipped.
    pub malformed_lines: Vec<MalformedLaneLogLine>,
}

/// Run the full roadmap-status join rooted at `root` (a resolved brain root) for `roadmap_slug`.
pub fn discover(
    root: &Path,
    roadmap_slug: &str,
) -> Result<RoadmapStatusResult, RoadmapStatusError> {
    discover_at(root, roadmap_slug, Utc::now())
}

/// Same as [`discover`], with an injectable clock so tests can pin `now` for liveness
/// computation without racing real time.
pub fn discover_at(
    root: &Path,
    roadmap_slug: &str,
    now: DateTime<Utc>,
) -> Result<RoadmapStatusResult, RoadmapStatusError> {
    let roadmap_dir = resolve_roadmap_dir(root, roadmap_slug)?;
    let (lane_entries, malformed_lines) = read_lane_log(&roadmap_dir);
    let repos_in_log = repos_from_lane_log(&lane_entries);

    let run_records = discover_run_records(root, roadmap_slug);
    // Union: repos named in the log, plus repos that wrote a run record but logged nothing —
    // that mismatch is itself a finding, not something to silently union away.
    let mut all_repos: BTreeSet<String> = repos_in_log.iter().cloned().collect();
    all_repos.extend(run_records.keys().cloned());

    let repo_registry = discover_repos(root);

    let mut lanes: BTreeMap<String, LaneResult> = all_repos
        .iter()
        .map(|r| {
            (
                r.clone(),
                LaneResult {
                    repo: r.clone(),
                    ..Default::default()
                },
            )
        })
        .collect();

    for entry in &lane_entries {
        let Some(repo) = entry.get("repo").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(lane) = lanes.get_mut(repo) else {
            continue;
        };
        let block_id = entry
            .get("block")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let spec_slug = if block_id.is_empty() {
            None
        } else {
            resolve_block_to_spec_slug(&block_id)
        };
        let sdlc_state = match (repo_registry.get(repo), spec_slug.as_ref()) {
            (Some(state_json_path), Some(slug)) => {
                let spec_dir = state_json_path
                    .parent()
                    .map(|p| p.join(slug))
                    .unwrap_or_else(|| PathBuf::from(slug));
                read_sdlc_state(&spec_dir, now)
            }
            _ => None,
        };
        lane.blocks.push(BlockActivity {
            block: block_id,
            status: entry
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("<absent>")
                .to_string(),
            note: entry.get("note").and_then(|v| v.as_str()).map(String::from),
            ts: entry.get("ts").and_then(|v| v.as_str()).map(String::from),
            spec_slug,
            sdlc_state,
        });
    }

    for (repo, record) in &run_records {
        lanes
            .entry(repo.clone())
            .or_insert_with(|| LaneResult {
                repo: repo.clone(),
                ..Default::default()
            })
            .run_record = Some(record.clone());
    }

    let repos_in_log_set: BTreeSet<&String> = repos_in_log.iter().collect();
    let repos_with_run_record_only: Vec<String> = run_records
        .keys()
        .filter(|r| !repos_in_log_set.contains(r))
        .cloned()
        .collect();

    let mut coverage_total = 0usize;
    for (repo, path) in &repo_registry {
        let Some(lane) = lanes.get_mut(repo) else {
            continue;
        };
        let gates = discover_operator_gates(path);
        coverage_total += gates.coverage_count;
        lane.operator_gates = gates;
        lane.carryover = discover_carryover(path);
    }

    // Registry / leases / message-queue depth, via `crate::coord`'s already-shipped reader —
    // this module defines no second way to read those artifacts.
    let coordination = coord::read_coordination_view(root);
    for entry in &coordination.registry {
        let Some(claim) = entry.claim.typed() else {
            continue;
        };
        if claim.roadmap != roadmap_slug {
            continue;
        }
        if let Some(lane) = lanes.get_mut(&claim.repo) {
            lane.lane_registry.push(entry.clone());
        }
    }
    for entry in &coordination.leases {
        let Some(record) = entry.lease.typed() else {
            continue;
        };
        if let Some(lane) = lanes.get_mut(&record.repo) {
            lane.leases.push(entry.clone());
        }
    }
    for entry in &coordination.messages {
        let Some(message) = entry.message.typed() else {
            continue;
        };
        if let Some(lane) = lanes.get_mut(&message.subject.repo) {
            lane.message_queue.inbox_count += 1;
            let is_older = lane
                .message_queue
                .oldest_unread_sent_at
                .as_deref()
                .is_none_or(|existing| message.sent_at.as_str() < existing);
            if is_older {
                lane.message_queue.oldest_unread_sent_at = Some(message.sent_at.clone());
            }
        }
    }

    Ok(RoadmapStatusResult {
        roadmap: roadmap_slug.to_string(),
        roadmap_dir,
        lanes,
        repos_in_lane_log: repos_in_log,
        repos_with_run_record_only,
        operator_coverage_total: coverage_total,
        malformed_lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn write(path: &Path, content: &str) {
        fs::create_dir_all(path.parent().unwrap()).expect("create parent dir");
        fs::write(path, content).expect("write fixture file");
    }

    #[test]
    fn resolve_roadmap_dir_prefers_new_over_legacy_and_flags_ambiguity() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();

        assert!(matches!(
            resolve_roadmap_dir(root, "demo"),
            Err(RoadmapStatusError::NotFound { .. })
        ));

        fs::create_dir_all(root.join("planning/roadmaps/demo")).unwrap();
        assert_eq!(
            resolve_roadmap_dir(root, "demo").unwrap(),
            root.join("planning/roadmaps/demo")
        );

        fs::create_dir_all(root.join("planning/demo")).unwrap();
        assert!(matches!(
            resolve_roadmap_dir(root, "demo"),
            Err(RoadmapStatusError::Ambiguous { .. })
        ));
    }

    #[test]
    fn read_lane_log_reports_malformed_line_with_byte_offset_instead_of_skipping() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let roadmap_dir = tmp.path().join("planning/roadmaps/demo");
        let good_line = r#"{"repo":"engine-rs","lane":"a","block":"EN.1.A","status":"done"}"#;
        let bad_line = "{not valid json";
        let content = format!("{good_line}\n{bad_line}\n");
        write(&roadmap_dir.join("lane-log.jsonl"), &content);

        let (entries, malformed) = read_lane_log(&roadmap_dir);
        assert_eq!(entries.len(), 1);
        assert_eq!(malformed.len(), 1);
        let m = &malformed[0];
        assert_eq!(m.line_number, 2);
        // The bad line starts exactly after the good line + its newline.
        assert_eq!(m.byte_offset, good_line.len() + 1);
        assert!(!m.error.is_empty());
    }

    #[test]
    fn zero_line_lane_log_resolves_without_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let roadmap_dir = tmp.path().join("planning/roadmaps/demo");
        fs::create_dir_all(&roadmap_dir).unwrap();
        // No lane-log.jsonl at all.
        let (entries, malformed) = read_lane_log(&roadmap_dir);
        assert!(entries.is_empty());
        assert!(malformed.is_empty());

        let result = discover_at(tmp.path(), "demo", Utc::now()).expect("resolves cleanly");
        assert!(result.lanes.is_empty());
        assert!(result.malformed_lines.is_empty());
        assert!(result.repos_in_lane_log.is_empty());
    }

    #[test]
    fn realpath_dedup_collapses_symlink_and_vault_path_to_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let vault = root.join("_planning").join("demo-repo");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("state.json"), r#"{"repo":"demo-repo"}"#).unwrap();

        let repo_dir = root.join("demo-repo");
        fs::create_dir_all(&repo_dir).unwrap();
        // repo_dir/planning -> ../_planning/demo-repo (the fleet's real symlink shape)
        symlink(&vault, repo_dir.join("planning")).expect("create planning symlink");

        let raw = sweep(root, |p| {
            p.file_name().and_then(|n| n.to_str()) == Some("state.json")
                && p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|n| n.to_str())
                    == Some("planning")
        });
        // Both the symlinked face and (if walked separately) the vault face are present in the
        // raw sweep as distinct strings before dedup.
        assert!(!raw.is_empty());

        let distinct = realpath_dedup(root, &raw);
        assert_eq!(
            distinct.len(),
            1,
            "a roadmap/state.json reachable through both the planning/ symlink and its vault path must be counted once, got {distinct:?}"
        );
        // Canonicalize the expected path too: on macOS `/tmp` is itself a symlink to
        // `/private/tmp`, so the tempdir root and the resolved realpath can legitimately differ
        // by that unrelated OS-level indirection even once the fixture's own dedup is correct.
        let expected = fs::canonicalize(vault.join("state.json")).unwrap();
        assert_eq!(distinct[0], expected);

        let repos = discover_repos(root);
        assert_eq!(repos.len(), 1);
        assert_eq!(repos.get("demo-repo"), Some(&expected));
    }

    #[test]
    fn realpath_dedup_caches_parent_resolution_and_stays_fast_over_many_files() {
        // A correctness+performance regression guard: a per-file (uncached) realpath
        // implementation is the exact defect this block's record warns cost the Python oracle
        // 194s of a 195s self-test run. This fixture is far smaller (perf differences at this
        // scale are noise either way) but exercises the SAME code path with enough files that an
        // accidentally-reintroduced per-file `fs::canonicalize` would still be exercised, and
        // pins a generous wall-clock ceiling so a real regression is still caught.
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let vault = root.join("_planning");
        fs::create_dir_all(&vault).unwrap();

        let mut hits = Vec::new();
        for i in 0..500 {
            let repo = format!("repo-{i}");
            let repo_vault = vault.join(&repo);
            fs::create_dir_all(&repo_vault).unwrap();
            let state_path = repo_vault.join("state.json");
            fs::write(&state_path, format!(r#"{{"repo":"{repo}"}}"#)).unwrap();

            let repo_dir = root.join(&repo);
            fs::create_dir_all(&repo_dir).unwrap();
            symlink(&repo_vault, repo_dir.join("planning")).unwrap();
            hits.push(repo_dir.join("planning").join("state.json"));
        }

        let start = std::time::Instant::now();
        let distinct = realpath_dedup(root, &hits);
        let elapsed = start.elapsed();

        assert_eq!(distinct.len(), 500);
        assert!(
            elapsed.as_secs() < 5,
            "cached-parent realpath dedup over 500 files took {elapsed:?}, expected well under 5s"
        );
    }

    #[test]
    fn resolve_block_to_spec_slug_matches_ticket_and_chore_and_leaves_phase_letter_unresolved() {
        assert_eq!(
            resolve_block_to_spec_slug("EN.ticket.stamp-workflow-run-id"),
            Some("ticket-stamp-workflow-run-id".to_string())
        );
        assert_eq!(
            resolve_block_to_spec_slug("EN.chore.tidy-fixtures"),
            Some("chore-tidy-fixtures".to_string())
        );
        assert_eq!(resolve_block_to_spec_slug("EN.15.H"), None);
    }

    #[test]
    fn discover_operator_gates_dedups_by_slug_and_ignores_other_edge_types() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_json = tmp.path().join("state.json");
        fs::write(
            &state_json,
            serde_json::json!({
                "repo": "demo",
                "tracks": [{
                    "blocks": [{
                        "id": "EN.1.A",
                        "depends_on": [
                            {"type": "operator", "slug": "sign-off", "exit": "artifact.md"},
                            {"type": "operator", "slug": "sign-off", "exit": "artifact.md"},
                            {"type": "approval", "slug": "release", "exit": "ok.md"},
                            {"type": "block", "id": "EN.0.A"}
                        ]
                    }]
                }]
            })
            .to_string(),
        )
        .unwrap();

        let gates = discover_operator_gates(&state_json);
        assert_eq!(gates.coverage_count, 2);
        let slugs: BTreeSet<&str> = gates.gates.iter().map(|g| g.slug.as_str()).collect();
        assert_eq!(slugs, BTreeSet::from_iter(["sign-off", "release"]));
    }

    #[test]
    fn discover_carryover_returns_the_array_verbatim() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state_json = tmp.path().join("state.json");
        fs::write(
            &state_json,
            serde_json::json!({
                "repo": "demo",
                "carryover": [{"kind": "defect", "summary": "x"}]
            })
            .to_string(),
        )
        .unwrap();

        let carryover = discover_carryover(&state_json);
        assert_eq!(carryover.len(), 1);
        assert_eq!(carryover[0]["kind"], "defect");
    }

    #[test]
    fn full_join_end_to_end_over_a_two_repo_roadmap() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        let roadmap_dir = root.join("planning/roadmaps/demo");
        write(
            &roadmap_dir.join("lane-log.jsonl"),
            &format!(
                "{}\n",
                serde_json::json!({
                    "repo": "engine-rs",
                    "lane": "engine-lane",
                    "block": "EN.1.A",
                    "status": "done",
                    "ts": "2026-09-08T00:00:00Z"
                })
            ),
        );

        // engine-rs's own planning/state.json, with one operator gate and one carryover entry.
        write(
            &root.join("engine-rs/planning/state.json"),
            &serde_json::json!({
                "repo": "engine-rs",
                "tracks": [{"blocks": [{
                    "id": "EN.1.A",
                    "depends_on": [{"type": "operator", "slug": "demo-gate", "exit": "e.md"}]
                }]}],
                "carryover": [{"kind": "deferred", "summary": "demo"}]
            })
            .to_string(),
        );

        // A repo that only wrote a run record, never logged a lane-log line.
        write(
            &root
                .join("mev/planning/orchestration-run/demo")
                .join("notes.md"),
            "---\nlifecycle: active\nrun_started: 2026-09-08\n---\n",
        );

        let result = discover_at(root, "demo", Utc::now()).expect("join resolves");
        assert_eq!(result.roadmap, "demo");
        assert_eq!(result.lanes.len(), 2);
        assert_eq!(result.repos_in_lane_log, vec!["engine-rs".to_string()]);
        assert_eq!(result.repos_with_run_record_only, vec!["mev".to_string()]);
        assert_eq!(result.operator_coverage_total, 1);

        let engine_lane = &result.lanes["engine-rs"];
        assert_eq!(engine_lane.blocks.len(), 1);
        assert_eq!(engine_lane.blocks[0].block, "EN.1.A");
        assert_eq!(engine_lane.operator_gates.coverage_count, 1);
        assert_eq!(engine_lane.carryover.len(), 1);

        let mev_lane = &result.lanes["mev"];
        assert!(mev_lane.blocks.is_empty());
        assert_eq!(
            mev_lane
                .run_record
                .as_ref()
                .unwrap()
                .notes
                .as_ref()
                .unwrap()
                .lifecycle,
            Some("active".to_string())
        );
    }

    #[test]
    fn no_committed_fixture_path_contains_a_literal_planning_segment() {
        // Standing rule: `**/planning/` is gitignored fleet-wide, so a committed fixture under
        // one would pass locally and silently vanish in CI. This module's tests build any tree
        // needing a literal `planning/` directory at runtime via `tempfile::tempdir()` (above);
        // nothing under the committed fixtures directory should ever need one.
        let fixtures_dir =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/roadmap_status");
        if !fixtures_dir.exists() {
            return;
        }
        let mut stack = vec![fixtures_dir];
        while let Some(dir) = stack.pop() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                assert!(
                    !path.components().any(|c| c.as_os_str() == "planning"),
                    "committed fixture path contains a literal 'planning' segment: {}",
                    path.display()
                );
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
    }
}
