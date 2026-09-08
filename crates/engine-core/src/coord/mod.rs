//! `coord` — a read-only reader for the fleet's coordination artifacts.
//!
//! EN.15.A task 1. The fleet has three liveness sources that disagree and nothing that reports
//! it (the registry, the lock-dir slot files, and a lane's own run-record `lifecycle`
//! frontmatter). This module is the ONE reader that joins them, using `okf-core`'s already-
//! shipped types (`okf_core::coord::*`, from `OK.6.A`) — it defines no record shape of its own.
//!
//! **Strictly read-only.** This module opens files and never writes one; the write path
//! belongs to `EN.15.C`. `crates/engine-serve/src/coordination.rs` (`EN.15.A` task 3) is the
//! only consumer that turns this into an HTTP response.
//!
//! ## On-disk layout (verified against the live tree 2026-09-08 — see the OK.6.A/EN.15.A block
//! records; do not re-derive this from first principles)
//!
//! - `<lock_dir>/lane-agents/*.json` — registry claims (`okf_core::coord::Registry`).
//! - `<lock_dir>/leases/*.json` — repo leases (`okf_core::coord::Lease`).
//! - `<lock_dir>/<repo>__agent-<agent>.json` — fleet-concurrency slots
//!   (`okf_core::coord::Slot`), **flat at the lock-dir root**. There is no `fleet-concurrency/`
//!   subdirectory; a reader that looks for one finds zero slots and reports a false-clean empty
//!   fleet, which is the exact failure class this module exists to prevent. Only files sitting
//!   directly in `<lock_dir>` are read as slots — a same-named file one level deeper (e.g. under
//!   an accidental `fleet-concurrency/` subdirectory) is never picked up.
//! - `<lock_dir>/queue/inbox/*.json` — cross-lane messages (`okf_core::coord::Message`).
//! - `<lock_dir>/commander-heartbeats/*.heartbeat` — raw-scalar heartbeat files
//!   (`okf_core::coord::HeartbeatRecord`), parsed via `HeartbeatValue::parse_raw`, never via
//!   `serde_json` (see `okf_core::coord::heartbeat`'s own doc comment).
//! - `<brain_root>/planning/roadmaps/<slug>/escalations.jsonl` — one `okf_core::coord::Escalation`
//!   per line, grouped by roadmap slug.
//! - `<brain_root>/**/planning/orchestration-run/<roadmap>/notes.md` — a lane's own "am I still
//!   running" record. Not an `okf-core` type (it is prose with YAML frontmatter, not a
//!   coordination JSON artifact) — read directly here as [`RunRecordEntry`]. The repo a run
//!   record belongs to is the path segment immediately before `planning`, mirroring
//!   `scripts/roadmap_status_discovery.py`'s `discover_run_records` in the brain repo.
//!
//! `<lock_dir>` resolves the same way the rest of the fleet does: `FLEET_LOCK_DIR` when set and
//! non-empty, else `<brain_root>/.fleet-locks`. This module adds no third resolution rule.
//!
//! ## Degraded is a first-class result, never a silent skip
//!
//! A record that cannot be read at all (I/O error), cannot be parsed as JSON at all (syntax
//! error), or parses only as `okf_core::coord::Coord::Legacy` (valid JSON, unknown/old shape) is
//! never dropped — it is recorded as a [`DegradationReason`] naming the offending path, and it
//! flips the whole [`CoordinationView::status`] to [`CoordinationStatus::Degraded`]. The same
//! applies to the cross-check between a run record and the registry: a run record whose
//! `lifecycle` is `active` but which has no matching registry claim (same repo, same roadmap)
//! reports `degraded`, never `live` — that gap is the spike finding this whole reader exists to
//! close.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use okf_core::{
    Coord, Escalation, HeartbeatRecord, HeartbeatValue, Lease, LeaseRecord, Message, Registry,
    RegistryClaim, Slot, SlotRecord,
};

/// The env var honoured as the first-precedence lock-dir override, mirroring
/// `base-template/scripts/fleet_concurrency_check.py`'s `--lock-dir` / `FLEET_LOCK_DIR`.
pub const FLEET_LOCK_DIR_ENV: &str = "FLEET_LOCK_DIR";

/// Name of the lock directory under the brain root when `FLEET_LOCK_DIR` is unset.
const LOCK_SUBDIR: &str = ".fleet-locks";

/// Overall health of a joined [`CoordinationView`].
///
/// Exhaustive: this is a plain two-state read-side classification (something didn't parse or
/// cross-check clean, or everything did) with no third state to soften into — a caller wanting
/// detail reads [`CoordinationView::degradation_reasons`], it never needs a wildcard arm here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CoordinationStatus {
    /// Every artifact read parsed into its strict typed shape and every cross-check agreed.
    Live,
    /// At least one artifact could not be read, could not be parsed, fell back to
    /// `Coord::Legacy`, or a cross-check (e.g. an active run record with no matching registry
    /// claim) disagreed. See `degradation_reasons` for the specifics.
    Degraded,
}

/// One reason `status` is [`CoordinationStatus::Degraded`], naming the offending path so an
/// operator (or the eventual `GET /api/coordination` caller) can go look at the actual file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DegradationReason {
    /// The artifact path this reason concerns. Not always a real filesystem path — the
    /// run-record/registry cross-check names a synthetic `repo:roadmap` locator instead, since
    /// no single file is "the" offender there.
    pub path: String,
    /// Human-readable explanation, safe to surface directly to an operator or over HTTP.
    pub reason: String,
}

impl DegradationReason {
    fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

/// A registry claim plus the path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegistryEntry {
    pub path: PathBuf,
    pub claim: Registry,
}

/// A repo lease plus the path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LeaseEntry {
    pub path: PathBuf,
    pub lease: Lease,
}

/// A fleet-concurrency slot plus the path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub path: PathBuf,
    pub slot: Slot,
}

/// A cross-lane message plus the path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MessageEntry {
    pub path: PathBuf,
    pub message: Message,
}

/// A commander heartbeat plus the path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatEntry {
    pub path: PathBuf,
    pub record: HeartbeatRecord,
}

/// One escalation line plus the roadmap slug and path it was read from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EscalationEntry {
    pub path: PathBuf,
    pub roadmap: String,
    pub escalation: Escalation,
}

/// A lane's own `planning/orchestration-run/<roadmap>/notes.md` run record — not an `okf-core`
/// coordination type (it's prose with YAML frontmatter), read directly by this module.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecordEntry {
    pub path: PathBuf,
    /// The repo this run record belongs to — the path segment immediately before `planning`.
    pub repo: String,
    /// The roadmap slug — the run record's own parent directory name.
    pub roadmap: String,
    /// The frontmatter `lifecycle:` value, when present.
    pub lifecycle: Option<String>,
}

/// The joined coordination view: every artifact class this reader knows about, plus an overall
/// [`CoordinationStatus`] and the [`DegradationReason`]s that produced it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinationView {
    pub status: CoordinationStatus,
    pub degradation_reasons: Vec<DegradationReason>,
    pub registry: Vec<RegistryEntry>,
    pub leases: Vec<LeaseEntry>,
    pub slots: Vec<SlotEntry>,
    pub messages: Vec<MessageEntry>,
    pub heartbeats: Vec<HeartbeatEntry>,
    pub escalations: Vec<EscalationEntry>,
    pub run_records: Vec<RunRecordEntry>,
}

/// Resolve the lock directory: `FLEET_LOCK_DIR` when set and non-empty, else
/// `<brain_root>/.fleet-locks`. Mirrors `fleet_concurrency_check.py`'s own precedence; this
/// module adds no third rule.
pub fn resolve_lock_dir(brain_root: &Path) -> PathBuf {
    match std::env::var(FLEET_LOCK_DIR_ENV) {
        Ok(raw) if !raw.trim().is_empty() => PathBuf::from(raw),
        _ => brain_root.join(LOCK_SUBDIR),
    }
}

/// Read the full joined coordination view rooted at `brain_root`. Never panics and never
/// silently drops a bad record — every failure to read or parse becomes a
/// [`DegradationReason`], and a missing lock directory or empty tree is reported `Live` with
/// nothing in it (there is no coordination activity yet, which is not itself a fault).
pub fn read_coordination_view(brain_root: &Path) -> CoordinationView {
    let lock_dir = resolve_lock_dir(brain_root);
    let mut reasons = Vec::new();

    let registry = read_registry(&lock_dir, &mut reasons);
    let leases = read_leases(&lock_dir, &mut reasons);
    let slots = read_slots(&lock_dir, &mut reasons);
    let messages = read_messages(&lock_dir, &mut reasons);
    let heartbeats = read_heartbeats(&lock_dir, &mut reasons);
    let escalations = read_escalations(brain_root, &mut reasons);
    let run_records = read_run_records(brain_root);

    check_active_run_records_against_registry(&run_records, &registry, &mut reasons);

    let status = if reasons.is_empty() {
        CoordinationStatus::Live
    } else {
        CoordinationStatus::Degraded
    };

    CoordinationView {
        status,
        degradation_reasons: reasons,
        registry,
        leases,
        slots,
        messages,
        heartbeats,
        escalations,
        run_records,
    }
}

/// List every regular file directly inside `dir` (never recursing into subdirectories) whose
/// extension matches `ext`. A missing directory yields an empty list, not an error — an
/// unpopulated coordination sub-tree is not itself a fault.
fn list_files_with_extension(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|e| e.to_str()) == Some(ext) {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// Read and parse `path` as JSON into `Coord<T>`. Records a [`DegradationReason`] (naming
/// `path`) and returns `None` on an I/O error or a JSON syntax error; records a reason but still
/// returns `Some` for a value that parses only as `Coord::Legacy` — that is a degraded record,
/// not an unreadable one, so callers that want the raw legacy JSON can still have it.
fn read_coord_json<T>(path: &Path, reasons: &mut Vec<DegradationReason>) -> Option<Coord<T>>
where
    T: serde::de::DeserializeOwned,
{
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            reasons.push(DegradationReason::new(
                path.display().to_string(),
                format!("could not read file: {e}"),
            ));
            return None;
        }
    };
    match serde_json::from_str::<Coord<T>>(&text) {
        Ok(coord) => {
            if coord.is_legacy() {
                reasons.push(DegradationReason::new(
                    path.display().to_string(),
                    "record did not match its strict typed shape (legacy/unrecognized format)",
                ));
            }
            Some(coord)
        }
        Err(e) => {
            reasons.push(DegradationReason::new(
                path.display().to_string(),
                format!("invalid JSON: {e}"),
            ));
            None
        }
    }
}

fn read_registry(lock_dir: &Path, reasons: &mut Vec<DegradationReason>) -> Vec<RegistryEntry> {
    list_files_with_extension(&lock_dir.join("lane-agents"), "json")
        .into_iter()
        .filter_map(|path| {
            let claim = read_coord_json::<RegistryClaim>(&path, reasons)?;
            Some(RegistryEntry { path, claim })
        })
        .collect()
}

fn read_leases(lock_dir: &Path, reasons: &mut Vec<DegradationReason>) -> Vec<LeaseEntry> {
    list_files_with_extension(&lock_dir.join("leases"), "json")
        .into_iter()
        .filter_map(|path| {
            let lease = read_coord_json::<LeaseRecord>(&path, reasons)?;
            Some(LeaseEntry { path, lease })
        })
        .collect()
}

/// Slot files sit FLAT at `<lock_dir>` itself — never in a `fleet-concurrency/` subdirectory.
/// `list_files_with_extension` never recurses, so a decoy file one level deeper is never picked
/// up here; that is the behavior under test, not an incidental side effect.
fn read_slots(lock_dir: &Path, reasons: &mut Vec<DegradationReason>) -> Vec<SlotEntry> {
    list_files_with_extension(lock_dir, "json")
        .into_iter()
        .filter_map(|path| {
            let slot = read_coord_json::<SlotRecord>(&path, reasons)?;
            Some(SlotEntry { path, slot })
        })
        .collect()
}

fn read_messages(lock_dir: &Path, reasons: &mut Vec<DegradationReason>) -> Vec<MessageEntry> {
    list_files_with_extension(&lock_dir.join("queue").join("inbox"), "json")
        .into_iter()
        .filter_map(|path| {
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    reasons.push(DegradationReason::new(
                        path.display().to_string(),
                        format!("could not read file: {e}"),
                    ));
                    return None;
                }
            };
            match serde_json::from_str::<Message>(&text) {
                Ok(message) => {
                    if message.is_legacy() {
                        reasons.push(DegradationReason::new(
                            path.display().to_string(),
                            "record did not match its strict typed shape (legacy/unrecognized format)",
                        ));
                    }
                    Some(MessageEntry { path, message })
                }
                Err(e) => {
                    reasons.push(DegradationReason::new(
                        path.display().to_string(),
                        format!("invalid JSON: {e}"),
                    ));
                    None
                }
            }
        })
        .collect()
}

/// Heartbeat files are raw scalar text, not JSON — parsed via `HeartbeatValue::parse_raw`,
/// which never fails (see `okf_core::coord::heartbeat`'s doc comment), so this path can only
/// ever add a degradation reason on an I/O error.
fn read_heartbeats(lock_dir: &Path, reasons: &mut Vec<DegradationReason>) -> Vec<HeartbeatEntry> {
    list_files_with_extension(&lock_dir.join("commander-heartbeats"), "heartbeat")
        .into_iter()
        .filter_map(|path| match fs::read_to_string(&path) {
            Ok(text) => {
                let value = HeartbeatValue::parse_raw(&text);
                Some(HeartbeatEntry {
                    path,
                    record: HeartbeatRecord { value, host: None },
                })
            }
            Err(e) => {
                reasons.push(DegradationReason::new(
                    path.display().to_string(),
                    format!("could not read file: {e}"),
                ));
                None
            }
        })
        .collect()
}

/// Read every `<brain_root>/planning/roadmaps/<slug>/escalations.jsonl`, one [`Escalation`] per
/// non-blank line. A missing `planning/roadmaps` directory, or a roadmap with no
/// `escalations.jsonl`, yields no entries and no degradation — there is simply nothing to
/// report yet.
fn read_escalations(
    brain_root: &Path,
    reasons: &mut Vec<DegradationReason>,
) -> Vec<EscalationEntry> {
    let roadmaps_dir = brain_root.join("planning").join("roadmaps");
    let mut out = Vec::new();
    let entries = match fs::read_dir(&roadmaps_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    let mut roadmap_dirs: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    roadmap_dirs.sort();

    for roadmap_dir in roadmap_dirs {
        let roadmap = match roadmap_dir.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let path = roadmap_dir.join("escalations.jsonl");
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for (line_no, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let line_path = format!("{}:{}", path.display(), line_no + 1);
            match serde_json::from_str::<Escalation>(line) {
                Ok(escalation) => {
                    if escalation.is_legacy() {
                        // The 24-line older family is EXPECTED to land here (see
                        // `okf_core::coord::escalation`'s doc comment) — that is not corruption,
                        // just an older shape, so it is not reported as a degradation reason.
                    }
                    out.push(EscalationEntry {
                        path: path.clone(),
                        roadmap: roadmap.clone(),
                        escalation,
                    });
                }
                Err(e) => {
                    reasons.push(DegradationReason::new(
                        line_path,
                        format!("invalid JSON: {e}"),
                    ));
                }
            }
        }
    }
    out
}

/// Directory names skipped while walking for `orchestration-run` — heavy or irrelevant
/// subtrees that a coordination reader has no business descending into.
const SKIP_DIR_NAMES: [&str; 5] = ["target", "node_modules", ".git", "dist", "build"];

/// Maximum walk depth from `brain_root` while looking for `orchestration-run` directories.
/// Bounds the walk on a real, deep fleet tree without needing symlink-cycle detection — every
/// real `orchestration-run` directory in this fleet sits within a handful of path segments of
/// its brain root (`<tier>/<repo>/planning/orchestration-run/...` at worst).
const RUN_RECORD_MAX_DEPTH: usize = 8;

/// Find every `planning/orchestration-run/<roadmap>/notes.md` under `brain_root`, however deep
/// it's nested, and parse each one's `lifecycle:` frontmatter field. A run record that can't be
/// read (I/O error, e.g. dangling symlink) is skipped, not degraded — the registry/run-record
/// cross-check below is the one that actually matters for `status`, and it only ever looks at
/// run records this function *did* manage to read.
fn read_run_records(brain_root: &Path) -> Vec<RunRecordEntry> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(brain_root.to_path_buf(), 0)];

    while let Some((dir, depth)) = stack.pop() {
        if depth > RUN_RECORD_MAX_DEPTH {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if SKIP_DIR_NAMES.contains(&name) {
                continue;
            }
            if name == "orchestration-run" {
                out.extend(read_roadmap_run_records(&path));
                continue;
            }
            stack.push((path, depth + 1));
        }
    }

    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Read every `<orchestration_run_dir>/<roadmap>/notes.md` beneath one `orchestration-run`
/// directory.
fn read_roadmap_run_records(orchestration_run_dir: &Path) -> Vec<RunRecordEntry> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(orchestration_run_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let roadmap_dir = entry.path();
        if !roadmap_dir.is_dir() {
            continue;
        }
        let Some(roadmap) = roadmap_dir.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let notes_path = roadmap_dir.join("notes.md");
        let Ok(text) = fs::read_to_string(&notes_path) else {
            continue;
        };
        let Some(repo) = repo_from_path(&notes_path) else {
            continue;
        };
        out.push(RunRecordEntry {
            path: notes_path,
            repo,
            roadmap: roadmap.to_string(),
            lifecycle: parse_frontmatter_lifecycle(&text),
        });
    }
    out
}

/// The repo a coordination-adjacent path belongs to: the path segment immediately before
/// `planning`, mirroring `scripts/roadmap_status_discovery.py`'s `discover_run_records` in the
/// brain repo (`agentic-portfolio`).
fn repo_from_path(path: &Path) -> Option<String> {
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

/// Extract the `lifecycle:` value from a `notes.md`'s YAML frontmatter (`---\n...\n---\n`),
/// mirroring `scripts/roadmap_status_discovery.py`'s `_parse_frontmatter`'s `key: value` line
/// scan — deliberately not a full YAML parser, since this frontmatter is always a flat map of
/// scalar values.
fn parse_frontmatter_lifecycle(text: &str) -> Option<String> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---")?;
    let frontmatter = &rest[..end];
    for line in frontmatter.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "lifecycle" {
                return Some(value.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

/// The cross-check the whole roadmap exists to make mechanical: a run record reporting
/// `lifecycle: active` must have a matching registry claim (same repo, same roadmap) — if it
/// doesn't, that lane is `degraded`, never `live`, however clean every individual file parsed.
fn check_active_run_records_against_registry(
    run_records: &[RunRecordEntry],
    registry: &[RegistryEntry],
    reasons: &mut Vec<DegradationReason>,
) {
    for record in run_records {
        if record.lifecycle.as_deref() != Some("active") {
            continue;
        }
        let has_matching_claim = registry.iter().any(|entry| {
            entry
                .claim
                .typed()
                .is_some_and(|claim| claim.repo == record.repo && claim.roadmap == record.roadmap)
        });
        if !has_matching_claim {
            reasons.push(DegradationReason::new(
                format!("{}:{}", record.repo, record.roadmap),
                format!(
                    "run record at {} reports lifecycle: active but no matching registry claim exists for repo '{}' roadmap '{}'",
                    record.path.display(),
                    record.repo,
                    record.roadmap
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `crates/engine-core/tests/fixtures/coord/<name>` — committed fixture trees. None of them
    /// contain a `planning` directory: `**/planning/` is gitignored fleet-wide (the real
    /// `planning/` is a symlink into the private HQ vault), so a fixture placed under one would
    /// pass locally and silently vanish in CI. Tests that need a `planning/...` path (run
    /// records, escalations) build one at runtime in a `tempfile::tempdir()` instead — see
    /// `write_run_record`/`write_escalations_jsonl` below.
    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/coord")
            .join(name)
    }

    /// Recursively collect every regular file under `dir`.
    fn walk_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk_files(&path, out);
            } else {
                out.push(path);
            }
        }
    }

    /// Write `<root>/<repo>/planning/orchestration-run/<roadmap>/notes.md` with a `lifecycle:`
    /// frontmatter field, mirroring the real fleet's run-record shape.
    fn write_run_record(root: &Path, repo: &str, roadmap: &str, lifecycle: &str) -> PathBuf {
        let dir = root
            .join(repo)
            .join("planning")
            .join("orchestration-run")
            .join(roadmap);
        fs::create_dir_all(&dir).expect("create run-record dir");
        let path = dir.join("notes.md");
        fs::write(
            &path,
            format!("---\nlifecycle: {lifecycle}\nrun_started: 2026-09-03\n---\n\n| # | Item |\n|---|---|\n| 1 | demo |\n"),
        )
        .expect("write notes.md");
        path
    }

    /// Write `<root>/planning/roadmaps/<roadmap>/escalations.jsonl` with one escalation line.
    fn write_escalations_jsonl(root: &Path, roadmap: &str, repo: &str) -> PathBuf {
        let dir = root.join("planning").join("roadmaps").join(roadmap);
        fs::create_dir_all(&dir).expect("create roadmap dir");
        let path = dir.join("escalations.jsonl");
        let line = serde_json::json!({
            "ts_utc": "2026-08-31T02:55:00Z",
            "repo": repo,
            "lane": repo,
            "kind": "disagreement",
            "severity": "blocking",
            "summary": "demo escalation.",
            "channel": format!("session:{repo}"),
            "gate_id": format!("{roadmap}/{repo}/EN.15.A"),
            "durable_home": {
                "channel": format!("session:{repo}"),
                "ref": format!("{repo}/planning/orchestration-run/{roadmap}/notes.md#session-1")
            },
            "verified_at_sha": "abc1234",
            "verified_by": "manual"
        });
        fs::write(&path, format!("{line}\n")).expect("write escalations.jsonl");
        path
    }

    /// Set `FLEET_LOCK_DIR` for the duration of `f`, restoring the previous value afterwards.
    /// Guarded by a mutex — `FLEET_LOCK_DIR` is process-global state, same discipline as
    /// `brain_root.rs`'s `ENGINE_BRAIN_ROOT` tests.
    fn with_fleet_lock_dir<T>(lock_dir: &Path, f: impl FnOnce() -> T) -> T {
        use std::sync::Mutex;
        static ENV_GUARD: Mutex<()> = Mutex::new(());
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(FLEET_LOCK_DIR_ENV).ok();
        std::env::set_var(FLEET_LOCK_DIR_ENV, lock_dir);

        let result = f();

        match previous {
            Some(v) => std::env::set_var(FLEET_LOCK_DIR_ENV, v),
            None => std::env::remove_var(FLEET_LOCK_DIR_ENV),
        }
        result
    }

    #[test]
    fn no_committed_fixture_path_contains_a_literal_planning_directory() {
        // `**/planning/` is gitignored fleet-wide (the module doc comment's own claim) — a
        // fixture placed under a directory literally named `planning` would commit fine locally
        // and then silently not exist once cloned in CI. Walk every committed file under
        // `tests/fixtures/coord/` and assert none of them sit under one.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/coord");
        let mut files = Vec::new();
        walk_files(&root, &mut files);
        assert!(
            !files.is_empty(),
            "expected committed coord fixtures under {}",
            root.display()
        );
        for file in &files {
            assert!(
                !file.components().any(|c| c.as_os_str() == "planning"),
                "fixture path lives under a `planning` directory (gitignored fleet-wide): {}",
                file.display()
            );
        }
    }

    #[test]
    fn healthy_tree_reports_live_with_every_category_populated() {
        // The `.fleet-locks/*` half is a committed fixture; the `planning/...` half (run record,
        // escalations) is built at runtime in a tempdir, since neither may live under a
        // directory literally named `planning` on disk (gitignored fleet-wide).
        let brain_root = tempfile::tempdir().expect("tempdir");
        write_run_record(brain_root.path(), "engine-rs", "demo-roadmap", "active");
        write_escalations_jsonl(brain_root.path(), "demo-roadmap", "engine-rs");

        let lock_dir = fixture("healthy").join(".fleet-locks");
        let view = with_fleet_lock_dir(&lock_dir, || read_coordination_view(brain_root.path()));

        assert_eq!(
            view.status,
            CoordinationStatus::Live,
            "{:?}",
            view.degradation_reasons
        );
        assert!(view.degradation_reasons.is_empty());
        assert_eq!(view.registry.len(), 1);
        assert_eq!(view.leases.len(), 1);
        assert_eq!(view.slots.len(), 1);
        assert_eq!(view.messages.len(), 1);
        assert_eq!(view.heartbeats.len(), 1);
        assert_eq!(view.escalations.len(), 1);
        assert_eq!(view.run_records.len(), 1);

        let claim = view.registry[0].claim.typed().expect("typed claim");
        assert_eq!(claim.repo, "engine-rs");
        assert_eq!(claim.roadmap, "demo-roadmap");

        let run_record = &view.run_records[0];
        assert_eq!(run_record.repo, "engine-rs");
        assert_eq!(run_record.roadmap, "demo-roadmap");
        assert_eq!(run_record.lifecycle.as_deref(), Some("active"));
    }

    #[test]
    fn corrupt_lane_agent_reports_degraded_naming_the_path() {
        let view = read_coordination_view(&fixture("corrupt_lane_agent"));

        assert_eq!(view.status, CoordinationStatus::Degraded);
        assert_eq!(
            view.registry.len(),
            0,
            "the corrupt claim must not be silently skipped in"
        );
        assert_eq!(view.degradation_reasons.len(), 1);
        let reason = &view.degradation_reasons[0];
        assert!(
            reason.path.ends_with("agent-broken.json"),
            "degradation reason must name the offending path, got: {}",
            reason.path
        );
    }

    #[test]
    fn active_run_record_with_no_registry_entry_reports_degraded_not_live() {
        // No `.fleet-locks` at all under this brain root, so the registry is trivially empty —
        // the point under test is the cross-check between an `active` run record and that empty
        // registry, not any lock-dir content.
        let brain_root = tempfile::tempdir().expect("tempdir");
        write_run_record(brain_root.path(), "mev", "some-roadmap", "active");

        let view = read_coordination_view(brain_root.path());

        assert_eq!(view.status, CoordinationStatus::Degraded);
        assert_eq!(view.registry.len(), 0);
        assert_eq!(view.run_records.len(), 1);
        assert_eq!(view.run_records[0].lifecycle.as_deref(), Some("active"));
        assert_eq!(view.degradation_reasons.len(), 1);
        assert!(view.degradation_reasons[0]
            .reason
            .contains("no matching registry claim"));
        assert!(view.degradation_reasons[0].path.contains("mev"));
    }

    #[test]
    fn slot_files_are_read_from_lock_dir_root_not_a_subdirectory() {
        let view = read_coordination_view(&fixture("slots_at_root_only"));

        // Exactly the one slot sitting at the lock-dir root is found; the decoy file one level
        // deeper under `fleet-concurrency/` must never be picked up.
        assert_eq!(view.slots.len(), 1);
        let slot = view.slots[0].slot.typed().expect("typed slot");
        assert_eq!(slot.repo, "engine-rs");
        assert!(
            !view.slots[0]
                .path
                .to_string_lossy()
                .contains("fleet-concurrency"),
            "slot must be read from the lock-dir root, not a fleet-concurrency/ subdirectory"
        );
        assert_eq!(view.status, CoordinationStatus::Live);
    }

    #[test]
    fn empty_tree_reports_live_with_nothing_in_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = read_coordination_view(dir.path());

        assert_eq!(view.status, CoordinationStatus::Live);
        assert!(view.degradation_reasons.is_empty());
        assert!(view.registry.is_empty());
        assert!(view.leases.is_empty());
        assert!(view.slots.is_empty());
        assert!(view.messages.is_empty());
        assert!(view.heartbeats.is_empty());
        assert!(view.escalations.is_empty());
        assert!(view.run_records.is_empty());
    }

    #[test]
    fn missing_lock_dir_is_reported_live_not_as_an_error() {
        // A brain root that exists but has never had `.fleet-locks` created is a legitimate
        // "nothing has run here yet" state, not corruption.
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(!dir.path().join(".fleet-locks").exists());

        let view = read_coordination_view(dir.path());
        assert_eq!(view.status, CoordinationStatus::Live);
        assert!(view.degradation_reasons.is_empty());
    }

    #[test]
    fn fleet_lock_dir_env_var_overrides_the_default_location() {
        let override_dir = fixture("healthy").join(".fleet-locks");

        // Brain root here is an unrelated empty tempdir — if the env var is honoured, escalations
        // and run records (which key off brain_root directly, not the lock dir) come back empty,
        // but the lock-dir-derived categories (registry/leases/slots/messages/heartbeats) still
        // resolve against `override_dir`.
        let unrelated_root = tempfile::tempdir().expect("tempdir");
        let view = with_fleet_lock_dir(&override_dir, || {
            read_coordination_view(unrelated_root.path())
        });

        assert_eq!(
            view.registry.len(),
            1,
            "FLEET_LOCK_DIR override was not honoured"
        );
        assert_eq!(view.slots.len(), 1);
        assert!(view.escalations.is_empty());
        assert!(view.run_records.is_empty());
    }

    #[test]
    fn resolve_lock_dir_defaults_to_brain_root_dot_fleet_locks() {
        use std::sync::Mutex;
        static ENV_GUARD: Mutex<()> = Mutex::new(());
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = std::env::var(FLEET_LOCK_DIR_ENV).ok();
        std::env::remove_var(FLEET_LOCK_DIR_ENV);

        let brain_root = PathBuf::from("/tmp/some-brain-root");
        let resolved = resolve_lock_dir(&brain_root);

        if let Some(v) = previous {
            std::env::set_var(FLEET_LOCK_DIR_ENV, v);
        }

        assert_eq!(resolved, brain_root.join(".fleet-locks"));
    }

    #[test]
    fn heartbeat_epoch_and_iso_formats_both_parse() {
        let view = read_coordination_view(&fixture("healthy"));
        assert_eq!(view.heartbeats.len(), 1);
        match &view.heartbeats[0].record.value {
            HeartbeatValue::Iso(s) => assert_eq!(s, "2026-09-03T09:12:25Z"),
            other => {
                panic!("expected an ISO heartbeat value in the healthy fixture, got {other:?}")
            }
        }
    }
}
