//! `coord::write` — the ONE seam every coordination write goes through.
//!
//! `EN.15.C` task 1. The sibling of the read-only reader in `coord/mod.rs` (`EN.15.A`): where
//! that module opens files and never writes one, this module is the only place in the crate
//! that does. Every route this block adds (`EN.15.C` tasks 2-6 — registry register/heartbeat/
//! release, lease/unlease, message send/drain/complete) goes through the functions here rather
//! than calling `fs::write` directly, so the three cross-cutting guarantees below cannot be
//! forgotten per-route.
//!
//! ## The seam, in order
//!
//! 1. **Schema-validate** the record — it must deserialize into its strict typed shape
//!    (`okf_core::coord::Coord<T>::Typed`), never fall back to `Coord::Legacy`.
//! 2. **Stamp `host`** — added to the record after validation (it is always an optional field,
//!    so adding it cannot turn a valid record invalid).
//! 3. **Snapshot** any file already at the target path to `<lock_dir>/.prev/<relative path>`,
//!    preserving the on-disk category layout (`lane-agents/…`, `leases/…`, `queue/…`) so two
//!    different record kinds that happen to share a filename can never collide in `.prev/`.
//! 4. **Write.**
//!
//! A failure at any step writes nothing — in particular, an invalid record never reaches the
//! snapshot or write steps, so an existing file at that path is left completely untouched.
//!
//! ## `host` is stamped, never enforced
//!
//! Fork 1 (`EN.15.C`'s block record) keeps this fleet on one host. `host` names which one wrote
//! a record so a later two-host world can fail loudly on a mismatch; nothing here refuses a
//! write on the basis of `host`, and it must stay that way until that later block exists.
//!
//! ## The two heartbeat shapes are different, and both are live
//!
//! The heartbeat **FILE** (`.fleet-locks/commander-heartbeats/*.heartbeat`) holds a bare
//! scalar — never JSON — and the live fleet writes epoch seconds today (see
//! `okf_core`'s `heartbeat` module doc comment). [`write_heartbeat_file`] writes that exact
//! shape via `HeartbeatValue::to_raw` and stamps no `host` (the file has no
//! such field; see that module's own reasoning). The heartbeat **FIELD** on a lane-agent
//! registry claim or a lease record is a completely different thing: an ISO-8601 string with
//! timezone, carried as ordinary JSON through [`write_coord_json`] like any other field.
//! Getting this collision wrong — writing an epoch integer into a JSON `heartbeat` field, or an
//! ISO string into a `.heartbeat` file — is flagged in the block record as the single easiest
//! defect here; the two are written through genuinely different functions so it cannot happen
//! by accident.
//!
//! ## Lock-dir resolution
//!
//! This module adds no lock-dir resolution rule of its own. Every function here takes an
//! already-resolved `lock_dir: &Path` — callers (the routes added in later tasks) resolve it
//! via the existing [`super::resolve_lock_dir`], exactly as the read side already does.

use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;

use okf_core::{Coord, HeartbeatValue, LeaseRecord, PidSource, RegistryClaim, SlotRecord};

/// Subdirectory under the lock dir holding pre-overwrite snapshots. Sits inside the
/// already-gitignored `.fleet-locks/` (see the block record's `notes`), so no `.gitignore`
/// change is needed and this is the local undo for a coordination write.
const PREV_SUBDIR: &str = ".prev";

/// Everything that can go wrong writing one coordination record through the seam.
#[derive(Debug, thiserror::Error)]
pub enum CoordWriteError {
    /// The record failed schema validation before anything was written — no snapshot was
    /// taken and no file was touched.
    #[error("invalid coordination record at {path}: {reason}")]
    Invalid { path: PathBuf, reason: String },
    /// An I/O error occurred while snapshotting the prior file or writing the new one.
    #[error("I/O error writing coordination record at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Write one coordination JSON record through the seam: schema-validate `value` against `T`,
/// stamp `host` onto it, snapshot any existing file at `path` to `.prev/`, then write.
///
/// `value` is the record as a raw [`serde_json::Value`] — the shape an HTTP route decodes a
/// request body into — rather than an already-typed `T`, precisely so an invalid value (one
/// that would fall back to [`Coord::Legacy`]) can be refused here instead of being unwritable
/// by construction. `path` should be a descendant of `lock_dir` (every coord write route
/// resolves its target that way); a `path` outside `lock_dir` still writes correctly, its
/// `.prev/` snapshot just falls back to the bare filename instead of mirroring a relative
/// subpath.
///
/// `host: None` writes the record with no `host` stamped at all (every coord record's `host`
/// field is optional and omitted-not-null on serialization, matching every `okf_core::coord`
/// type's own round-trip tests).
pub fn write_coord_json<T>(
    lock_dir: &Path,
    path: &Path,
    mut value: serde_json::Value,
    host: Option<&str>,
) -> Result<(), CoordWriteError>
where
    T: DeserializeOwned,
{
    // 1. Schema-validate: `value` must deserialize into `Coord<T>::Typed`, never fall back to
    // `Coord::Legacy` (a value that doesn't even parse as valid JSON via `from_value` can't
    // happen here since we already hold a `serde_json::Value`; the only failure mode is a
    // shape mismatch, i.e. landing in `Legacy`).
    let coord: Coord<T> =
        serde_json::from_value(value.clone()).map_err(|e| CoordWriteError::Invalid {
            path: path.to_path_buf(),
            reason: format!("record does not deserialize into its typed shape: {e}"),
        })?;
    if coord.is_legacy() {
        return Err(CoordWriteError::Invalid {
            path: path.to_path_buf(),
            reason: "record did not match its strict typed shape (legacy/unrecognized format)"
                .to_string(),
        });
    }

    // 2. Stamp `host` — after validation, since it's always optional and cannot invalidate an
    // already-valid record.
    if let Some(h) = host {
        if let Some(obj) = value.as_object_mut() {
            obj.insert("host".to_string(), serde_json::Value::String(h.to_string()));
        }
    }

    // 3. Snapshot any existing file before it is overwritten.
    snapshot_existing(lock_dir, path)?;

    // 4. Write.
    let text = serde_json::to_string_pretty(&value).map_err(|e| CoordWriteError::Invalid {
        path: path.to_path_buf(),
        reason: format!("record failed to serialize: {e}"),
    })?;
    write_text(path, &text)
}

/// Write a raw commander-heartbeat FILE — a bare scalar, never JSON — through the same
/// snapshot discipline as [`write_coord_json`]. No `host` is stamped (the file itself carries
/// no such field, per `okf_core`'s `heartbeat` module doc comment) and no schema validation
/// step applies: [`HeartbeatValue::parse_raw`] never fails on read, and every [`HeartbeatValue`]
/// round-trips through [`HeartbeatValue::to_raw`] by construction, so the only way this can
/// fail is an I/O error.
pub fn write_heartbeat_file(
    lock_dir: &Path,
    path: &Path,
    value: &HeartbeatValue,
) -> Result<(), CoordWriteError> {
    snapshot_existing(lock_dir, path)?;
    write_text(path, &value.to_raw())
}

/// Create `path`'s parent directory if needed and write `text` to it.
fn write_text(path: &Path, text: &str) -> Result<(), CoordWriteError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| CoordWriteError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
    }
    fs::write(path, text).map_err(|e| CoordWriteError::Io {
        path: path.to_path_buf(),
        source: e,
    })
}

/// Copy `path`, if it exists, to `<lock_dir>/.prev/<path relative to lock_dir>` before it gets
/// overwritten. A missing `path` is a no-op — there is nothing to snapshot before a record's
/// first write. When `path` does not sit under `lock_dir`, the snapshot falls back to
/// `<lock_dir>/.prev/<basename>` rather than failing outright.
fn snapshot_existing(lock_dir: &Path, path: &Path) -> Result<(), CoordWriteError> {
    if !path.exists() {
        return Ok(());
    }
    let rel: &Path = match path.strip_prefix(lock_dir) {
        Ok(rel) => rel,
        Err(_) => path.file_name().map(Path::new).unwrap_or(path),
    };
    let prev_path = lock_dir.join(PREV_SUBDIR).join(rel);
    if let Some(parent) = prev_path.parent() {
        fs::create_dir_all(parent).map_err(|e| CoordWriteError::Io {
            path: prev_path.clone(),
            source: e,
        })?;
    }
    fs::copy(path, &prev_path).map_err(|e| CoordWriteError::Io {
        path: prev_path.clone(),
        source: e,
    })?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Registry verbs — register / heartbeat / release. `EN.15.C` task 2.
// ---------------------------------------------------------------------------------------------

/// Heavy-lane category capacity, mirroring `base-template/scripts/fleet_concurrency_check.py`'s
/// `MAX_LANES_BY_CATEGORY` (D66). Kept as a small Rust table rather than read from the sibling
/// repo's script at runtime (production code has no dependency on a sibling checkout being
/// present); the parity test in this module's `tests` block cross-checks the REAL Python's
/// behaviour empirically at test time rather than trusting this table blindly.
const MAX_LANES_BY_CATEGORY: &[(&str, usize)] = &[("browser-automation", 2), ("native-build", 4)];

/// The default cap for a category this table doesn't name — mirrors
/// `MAX_LANES_BY_CATEGORY.get(category, MAX_HEAVY_LANES)`, where `MAX_HEAVY_LANES` is the
/// Python's `browser-automation` cap.
fn cap_for_category(category: &str) -> usize {
    MAX_LANES_BY_CATEGORY
        .iter()
        .find(|(name, _)| *name == category)
        .map(|(_, cap)| *cap)
        .unwrap_or(MAX_LANES_BY_CATEGORY[0].1)
}

/// Filesystem-safe stand-in for a repo/agent name used in a lock filename — mirrors
/// `fleet_concurrency_check.py`'s `_safe_repo_name`.
fn safe_component(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// `<lock_dir>/lane-agents/agent-<name>.json` — the lane-agent registry claim path for
/// `agent_name`.
fn registry_claim_path(lock_dir: &Path, agent_name: &str) -> PathBuf {
    lock_dir
        .join("lane-agents")
        .join(format!("agent-{}.json", safe_component(agent_name)))
}

/// `<lock_dir>/<repo>__agent-<agent>.json`, flat at the lock-dir root — the exact filename
/// scheme `fleet_concurrency_check.py`'s `_lock_path` produces when an `--agent` is supplied,
/// so a slot this seam writes and one the Python writes can supersede/heartbeat each other.
fn slot_path(lock_dir: &Path, repo: &str, agent_name: &str) -> PathBuf {
    lock_dir.join(format!(
        "{}__agent-{}.json",
        safe_component(repo),
        safe_component(agent_name)
    ))
}

/// Read `path` as `Coord<T>` and return the typed record, or `None` on any I/O error, JSON
/// error, or a value that only parses as `Coord::Legacy`. Used by `register`/`heartbeat` to
/// read an EXISTING record before refreshing it — never surfaced as a [`CoordWriteError`],
/// since "no prior record" and "prior record didn't parse" are both legitimately "start fresh"
/// for a `register` call, and `heartbeat` handles the "nothing to heartbeat" case itself.
fn read_typed<T>(path: &Path) -> Option<T>
where
    T: DeserializeOwned + Clone,
{
    let text = fs::read_to_string(path).ok()?;
    let coord: Coord<T> = serde_json::from_str(&text).ok()?;
    coord.typed().cloned()
}

/// Every non-stale slot record at `<lock_dir>/*.json` (flat root, never recursing) whose
/// `category` matches, alongside the path it was read from — sorted by path, matching
/// `fleet_concurrency_check.py`'s `sorted(lock_dir.glob("*.json"))` iteration order so the two
/// sides' `active` listings agree. This function does no TTL/staleness sweep of its own: every
/// entry a real register call in this seam's tests produces is fresh, and staleness sweeping is
/// Fork 2's Python-side job (`out_of_scope`), not duplicated here.
fn category_slot_entries(lock_dir: &Path, category: &str) -> Vec<(PathBuf, SlotRecord)> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(lock_dir) {
        Ok(e) => e,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(slot) = read_typed::<SlotRecord>(&path) {
            if slot.category == category {
                out.push((path, slot));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Everything a `register` call needs. The block's `what` folds the lane registry's own
/// `register` and the fleet-concurrency "slot register" into the SAME HTTP route — `EN.15.C`
/// exposes exactly eight POST routes (register/heartbeat/release/lease/unlease/send/drain/
/// complete), not nine — so this one function writes the lane-agent registry claim and, only
/// when `category` is `Some`, also enforces and writes the heavy-lane capacity slot.
pub struct RegisterRequest<'a> {
    pub agent_name: &'a str,
    pub repo: &'a str,
    pub lane: &'a str,
    pub roadmap: &'a str,
    pub host: Option<&'a str>,
    /// Heavy-lane category (`"browser-automation"` / `"native-build"`) this repo is gated
    /// under, or `None` for a light repo carrying no capacity gate. Determining the category
    /// from a repo's `harness.json` (`fleet_concurrency_check.py is-heavy`) is out of scope for
    /// this function, which only enforces the cap once told the category.
    pub category: Option<&'a str>,
    /// ISO-8601 timestamp with timezone — stamped onto the registry claim's `started_at` (first
    /// register only; a repeat register leaves it untouched) and `heartbeat` (every register).
    pub now_iso: &'a str,
    /// Epoch seconds — stamped onto the slot's `started_at`, mirroring `time.time()`. Refreshed
    /// on every register, including a repeat one, matching the Python's own heartbeat-via-
    /// re-register semantics for a slot.
    pub now_epoch: f64,
    /// The pid stamped onto a written slot. `pid_source` is always `PidSource::OwnProcess`
    /// (the Python's `"self"`) here — this seam has no notion of vouching for a caller-supplied
    /// external pid the way the Python's `--pid` flag does.
    pub pid: i64,
}

/// The result of a `register` call: refused (at capacity for `category`) or allowed, with the
/// same `reason`/`active` shape `fleet_concurrency_check.py`'s `LockResult` reports so a caller
/// can render either side identically.
#[derive(Debug, Clone, PartialEq)]
pub struct RegisterOutcome {
    pub allowed: bool,
    pub reason: Option<String>,
    pub active: Vec<String>,
}

/// `register` — write the lane-agent registry claim, and (when `req.category` is `Some`)
/// enforce and write the heavy-lane capacity slot first. A capacity refusal writes NOTHING —
/// neither the slot nor the registry claim — mirroring the Python's own all-or-nothing
/// `register()`, which never writes a lock file on a refusal.
pub fn register(
    lock_dir: &Path,
    req: &RegisterRequest,
) -> Result<RegisterOutcome, CoordWriteError> {
    if let Some(category) = req.category {
        let this_slot_path = slot_path(lock_dir, req.repo, req.agent_name);
        let cap = cap_for_category(category);
        let category_survivors = category_slot_entries(lock_dir, category);
        let already_registered = category_survivors
            .iter()
            .any(|(path, _)| path == &this_slot_path);
        let active_repos: Vec<String> = category_survivors
            .iter()
            .map(|(_, slot)| slot.repo.clone())
            .collect();

        if !already_registered && active_repos.len() >= cap {
            return Ok(RegisterOutcome {
                allowed: false,
                reason: Some(format!(
                    "fleet at capacity for '{category}' ({}/{cap} lanes active): {}",
                    active_repos.len(),
                    active_repos.join(", ")
                )),
                active: active_repos,
            });
        }

        let slot = SlotRecord {
            repo: req.repo.to_string(),
            pid: req.pid,
            pid_source: PidSource::OwnProcess,
            agent: Some(req.agent_name.to_string()),
            category: category.to_string(),
            started_at: req.now_epoch,
            host: None, // stamped by write_coord_json below.
        };
        let value = serde_json::to_value(&slot).map_err(|e| CoordWriteError::Invalid {
            path: this_slot_path.clone(),
            reason: format!("slot record failed to serialize: {e}"),
        })?;
        write_coord_json::<SlotRecord>(lock_dir, &this_slot_path, value, req.host)?;
    }

    let claim_path = registry_claim_path(lock_dir, req.agent_name);
    let existing_claim = read_typed::<RegistryClaim>(&claim_path);
    let claim = RegistryClaim {
        agent_name: req.agent_name.to_string(),
        repo: req.repo.to_string(),
        lane: req.lane.to_string(),
        roadmap: req.roadmap.to_string(),
        // `started_at` is an acquisition timestamp, set once: a repeat register keeps the
        // FIRST claim's value rather than re-stamping it.
        started_at: existing_claim
            .as_ref()
            .map(|c| c.started_at.clone())
            .unwrap_or_else(|| req.now_iso.to_string()),
        heartbeat: req.now_iso.to_string(),
        current_block: existing_claim
            .as_ref()
            .and_then(|c| c.current_block.clone()),
        block_started_at: existing_claim
            .as_ref()
            .and_then(|c| c.block_started_at.clone()),
        host: None, // stamped by write_coord_json below.
    };
    let value = serde_json::to_value(&claim).map_err(|e| CoordWriteError::Invalid {
        path: claim_path.clone(),
        reason: format!("registry claim failed to serialize: {e}"),
    })?;
    write_coord_json::<RegistryClaim>(lock_dir, &claim_path, value, req.host)?;

    Ok(RegisterOutcome {
        allowed: true,
        reason: None,
        active: Vec::new(),
    })
}

/// A `heartbeat` call: re-stamp ONLY the registry claim's `heartbeat` field (plus
/// `current_block`/`block_started_at` when the caller passes them) — `started_at` is left
/// completely untouched, unlike `register`'s own refresh path. Errors when no existing claim
/// exists for `agent_name`: there is nothing to heartbeat, and heartbeating one into existence
/// would silently paper over a lane that never registered.
pub struct HeartbeatRequest<'a> {
    pub agent_name: &'a str,
    pub host: Option<&'a str>,
    pub now_iso: &'a str,
    pub current_block: Option<&'a str>,
    pub block_started_at: Option<&'a str>,
}

pub fn heartbeat(lock_dir: &Path, req: &HeartbeatRequest) -> Result<(), CoordWriteError> {
    let claim_path = registry_claim_path(lock_dir, req.agent_name);
    let Some(existing) = read_typed::<RegistryClaim>(&claim_path) else {
        return Err(CoordWriteError::Invalid {
            path: claim_path,
            reason: format!(
                "no existing registry claim for agent `{}` to heartbeat",
                req.agent_name
            ),
        });
    };
    let claim = RegistryClaim {
        agent_name: existing.agent_name,
        repo: existing.repo,
        lane: existing.lane,
        roadmap: existing.roadmap,
        started_at: existing.started_at, // untouched — an acquisition timestamp, set once.
        heartbeat: req.now_iso.to_string(),
        current_block: req
            .current_block
            .map(|s| s.to_string())
            .or(existing.current_block),
        block_started_at: req
            .block_started_at
            .map(|s| s.to_string())
            .or(existing.block_started_at),
        host: None, // stamped by write_coord_json below.
    };
    let value = serde_json::to_value(&claim).map_err(|e| CoordWriteError::Invalid {
        path: claim_path.clone(),
        reason: format!("registry claim failed to serialize: {e}"),
    })?;
    write_coord_json::<RegistryClaim>(lock_dir, &claim_path, value, req.host)
}

/// `release` — remove the lane-agent registry claim for `agent_name`. Idempotent: an
/// already-absent claim returns `Ok(false)` rather than an error, mirroring
/// `fleet_concurrency_check.py release`'s always-succeeds, `removed`-flag-tells-you-if-anything-
/// happened contract.
pub fn release(lock_dir: &Path, agent_name: &str) -> Result<bool, CoordWriteError> {
    let claim_path = registry_claim_path(lock_dir, agent_name);
    let existed = claim_path.exists();
    if existed {
        fs::remove_file(&claim_path).map_err(|e| CoordWriteError::Io {
            path: claim_path,
            source: e,
        })?;
    }
    Ok(existed)
}

// ---------------------------------------------------------------------------------------------
// Lease verbs — lease / unlease. `EN.15.C` task 3.
// ---------------------------------------------------------------------------------------------

/// `<lock_dir>/leases/lease-<repo>.json` — the exact directory
/// `mev::brain::lease::check_quiesce` reads (`.claude/workflows/lease.schema.json`'s
/// `leases/` subdir), so a lease written here is indistinguishable, to mev, from one written by
/// hand.
fn lease_path(lock_dir: &Path, repo: &str) -> PathBuf {
    lock_dir
        .join("leases")
        .join(format!("lease-{}.json", safe_component(repo)))
}

/// Everything a `lease` call needs to acquire (or renew) an exclusive/shared claim on `repo`'s
/// working tree.
pub struct LeaseRequest<'a> {
    pub repo: &'a str,
    pub lane: &'a str,
    /// The `ListAgents` nickname taking this lease — the SAME `agent` field
    /// `mev::brain::lease::check_quiesce`'s self-exemption compares against, and the same one
    /// `EN.15.B` threads onto `EmitStateNode`/`CloseBlockNode` from
    /// `engine-serve/src/workflows.rs:429`'s `sdlc_event.agent`. This seam introduces no second
    /// identity concept — a caller passes whatever string it would otherwise pass to those
    /// nodes.
    pub agent: &'a str,
    pub kind: okf_core::LeaseKind,
    /// OPTIONAL. Absent means `LeaseScope::Repo`, matching the schema's own documented default.
    pub scope: Option<okf_core::LeaseScope>,
    pub host: Option<&'a str>,
    /// ISO-8601 timestamp with timezone — stamped onto `acquired_at` on a FIRST lease (a
    /// renewal keeps the existing `acquired_at`) and onto `heartbeat` on every call, matching
    /// `register`'s own acquisition-timestamp-set-once/heartbeat-every-call semantics.
    pub now_iso: &'a str,
    /// The per-block window this lease's exclusivity claims, mirroring `MV.20.C`'s lane-record
    /// `exclusive_repos: [{repo, blocks}]` window shape. `None` (or an empty slice) is a
    /// whole-lane lease claiming no window — parity with that shape's plain string form, which
    /// names no blocks at all. Every block named here is checked against `lane_blocks` BEFORE
    /// anything is written; this is request-time validation only — `okf_core::LeaseRecord`
    /// carries no `window` field of its own (`OK.6.A`), so nothing here is persisted to disk.
    pub window: Option<&'a [String]>,
    /// The full set of block ids the lane actually owns (its own `blocks[]`) — the yardstick
    /// `window` is checked against. Caller-supplied: this seam has no reader of its own for a
    /// lane record.
    pub lane_blocks: &'a [String],
}

/// `lease` — acquire or renew an exclusive/shared claim on `req.repo`'s working tree. A
/// `req.window` naming a block absent from `req.lane_blocks` is refused, naming the absent
/// block, BEFORE anything is written — mirroring every other refusal in this seam ("a failure
/// at any step writes nothing").
pub fn lease(lock_dir: &Path, req: &LeaseRequest) -> Result<(), CoordWriteError> {
    let path = lease_path(lock_dir, req.repo);
    if let Some(window) = req.window {
        for block in window {
            if !req.lane_blocks.iter().any(|b| b == block) {
                return Err(CoordWriteError::Invalid {
                    path,
                    reason: format!(
                        "lease window names block `{block}`, which is absent from the lane's \
                         blocks[]"
                    ),
                });
            }
        }
    }

    let existing = read_typed::<LeaseRecord>(&path);
    let record = LeaseRecord {
        repo: req.repo.to_string(),
        lane: req.lane.to_string(),
        agent: req.agent.to_string(),
        // `acquired_at` is an acquisition timestamp, set once: a renewal keeps the FIRST
        // lease's value rather than re-stamping it — same discipline as `register`'s
        // `started_at`.
        acquired_at: existing
            .as_ref()
            .map(|l| l.acquired_at.clone())
            .unwrap_or_else(|| req.now_iso.to_string()),
        kind: req.kind,
        heartbeat: Some(req.now_iso.to_string()),
        scope: req.scope,
        host: None, // stamped by write_coord_json below.
    };
    let value = serde_json::to_value(&record).map_err(|e| CoordWriteError::Invalid {
        path: path.clone(),
        reason: format!("lease record failed to serialize: {e}"),
    })?;
    write_coord_json::<LeaseRecord>(lock_dir, &path, value, req.host)
}

/// `unlease` — release the lease on `repo`, if any. Idempotent: an already-absent lease
/// returns `Ok(false)` rather than an error, matching `release`'s own always-succeeds contract.
pub fn unlease(lock_dir: &Path, repo: &str) -> Result<bool, CoordWriteError> {
    let path = lease_path(lock_dir, repo);
    let existed = path.exists();
    if existed {
        fs::remove_file(&path).map_err(|e| CoordWriteError::Io { path, source: e })?;
    }
    Ok(existed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease_json(heartbeat: &str) -> serde_json::Value {
        serde_json::json!({
            "repo": "engine-rs",
            "lane": "engine-rs",
            "agent": "engine-rs-1",
            "acquired_at": "2026-09-08T10:00:00Z",
            "kind": "exclusive",
            "heartbeat": heartbeat,
        })
    }

    fn registry_json() -> serde_json::Value {
        serde_json::json!({
            "agent_name": "engine-rs-1",
            "repo": "engine-rs",
            "lane": "engine-rs",
            "roadmap": "coordination-layer-port",
            "started_at": "2026-09-08T09:00:00Z",
            "heartbeat": "2026-09-08T09:05:00Z",
        })
    }

    #[test]
    fn write_with_no_prior_file_leaves_no_prev_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let path = lock_dir.join("lane-agents").join("agent-x.json");

        write_coord_json::<RegistryClaim>(lock_dir, &path, registry_json(), Some("brain-mini"))
            .expect("write should succeed");

        assert!(path.exists());
        let prev_path = lock_dir
            .join(".prev")
            .join("lane-agents")
            .join("agent-x.json");
        assert!(
            !prev_path.exists(),
            "no prior file existed, so no .prev/ entry should have been written"
        );
    }

    #[test]
    fn second_write_leaves_a_readable_prior_copy_of_the_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let path = lock_dir.join("lane-agents").join("agent-x.json");

        write_coord_json::<RegistryClaim>(lock_dir, &path, registry_json(), Some("brain-mini"))
            .expect("first write should succeed");
        let first_written = fs::read_to_string(&path).expect("read first write");

        let mut second = registry_json();
        second["heartbeat"] = serde_json::Value::String("2026-09-08T09:10:00Z".to_string());
        write_coord_json::<RegistryClaim>(lock_dir, &path, second, Some("brain-mini"))
            .expect("second write should succeed");

        let prev_path = lock_dir
            .join(".prev")
            .join("lane-agents")
            .join("agent-x.json");
        assert!(prev_path.exists(), "expected a .prev/ snapshot to exist");
        let snapshot = fs::read_to_string(&prev_path).expect("read snapshot");
        assert_eq!(
            snapshot, first_written,
            ".prev/ snapshot must hold the FIRST write's content"
        );

        // The live file now holds the second write, not the first.
        let live = fs::read_to_string(&path).expect("read live file");
        assert_ne!(live, first_written);
    }

    #[test]
    fn invalid_record_writes_nothing_and_leaves_existing_file_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let path = lock_dir.join("lane-agents").join("agent-x.json");

        write_coord_json::<RegistryClaim>(lock_dir, &path, registry_json(), Some("brain-mini"))
            .expect("valid write should succeed");
        let before = fs::read_to_string(&path).expect("read before");

        // Missing the required `heartbeat` field -> falls back to Coord::Legacy -> refused.
        let invalid = serde_json::json!({
            "agent_name": "engine-rs-1",
            "repo": "engine-rs",
            "lane": "engine-rs",
            "roadmap": "coordination-layer-port",
            "started_at": "2026-09-08T09:00:00Z",
        });
        let err = write_coord_json::<RegistryClaim>(lock_dir, &path, invalid, Some("brain-mini"))
            .expect_err("invalid record must be refused");
        assert!(matches!(err, CoordWriteError::Invalid { .. }));

        let after = fs::read_to_string(&path).expect("read after");
        assert_eq!(before, after, "existing file must be left untouched");

        let prev_path = lock_dir
            .join(".prev")
            .join("lane-agents")
            .join("agent-x.json");
        assert!(
            !prev_path.exists(),
            "an invalid write must not even reach the snapshot step"
        );
    }

    #[test]
    fn written_record_carries_host_and_nothing_refuses_on_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();
        let path = lock_dir.join("leases").join("lease-engine-rs.json");

        write_coord_json::<LeaseRecord>(
            lock_dir,
            &path,
            lease_json("2026-09-08T10:05:00Z"),
            Some("brain-mini"),
        )
        .expect("write should succeed regardless of host");

        let text = fs::read_to_string(&path).expect("read written record");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse written JSON");
        assert_eq!(
            value.get("host").and_then(|h| h.as_str()),
            Some("brain-mini"),
            "written record must carry the stamped host"
        );

        // A write with no host produces a record that omits the field entirely (never null).
        let path2 = lock_dir.join("leases").join("lease-mev.json");
        write_coord_json::<LeaseRecord>(lock_dir, &path2, lease_json("2026-09-08T10:06:00Z"), None)
            .expect("write with no host should succeed");
        let text2 = fs::read_to_string(&path2).expect("read written record 2");
        let value2: serde_json::Value = serde_json::from_str(&text2).expect("parse written JSON 2");
        assert!(
            value2.as_object().unwrap().get("host").is_none(),
            "no host was stamped, so the field must be absent, not null"
        );
    }

    /// The load-bearing collision test: a heartbeat FILE (raw scalar, epoch) and a heartbeat
    /// FIELD on a JSON record (ISO-8601 with timezone) are written through genuinely different
    /// functions here, and neither format leaks into the other.
    #[test]
    fn heartbeat_file_is_epoch_and_heartbeat_field_is_iso_with_timezone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        // The heartbeat FILE: epoch seconds, raw text, no JSON, no host.
        let heartbeat_path = lock_dir
            .join("commander-heartbeats")
            .join("brain-main.heartbeat");
        write_heartbeat_file(
            lock_dir,
            &heartbeat_path,
            &HeartbeatValue::Epoch(1_787_478_266),
        )
        .expect("heartbeat file write should succeed");
        let raw = fs::read_to_string(&heartbeat_path).expect("read heartbeat file");
        assert_eq!(
            raw, "1787478266",
            "heartbeat FILE must be written as bare epoch-seconds text, not JSON"
        );
        assert!(
            raw.parse::<i64>().is_ok(),
            "heartbeat FILE content must parse as a plain integer"
        );

        // The heartbeat FIELD on a lane-agent registry claim: ISO-8601 with timezone, ordinary
        // JSON, alongside a stamped host.
        let claim_path = lock_dir.join("lane-agents").join("agent-y.json");
        write_coord_json::<RegistryClaim>(
            lock_dir,
            &claim_path,
            registry_json(),
            Some("brain-mini"),
        )
        .expect("registry claim write should succeed");
        let claim_text = fs::read_to_string(&claim_path).expect("read claim");
        let claim_value: serde_json::Value =
            serde_json::from_str(&claim_text).expect("parse claim JSON");
        let heartbeat_field = claim_value
            .get("heartbeat")
            .and_then(|h| h.as_str())
            .expect("heartbeat field must be a JSON string");
        assert_eq!(heartbeat_field, "2026-09-08T09:05:00Z");
        assert!(
            heartbeat_field.parse::<i64>().is_err(),
            "heartbeat FIELD must never be a bare epoch integer"
        );
        assert!(
            heartbeat_field.ends_with('Z') || heartbeat_field.contains('+'),
            "heartbeat FIELD must carry a timezone, ISO-8601 style"
        );
    }

    #[test]
    fn seam_resolves_lock_dir_via_the_existing_resolve_lock_dir_adding_no_second_rule() {
        // No FLEET_LOCK_DIR override set: `resolve_lock_dir` must fall back to
        // `<brain_root>/.fleet-locks`, and every function in this module accepts that exact
        // resolved directory as `lock_dir` with no resolution logic of its own.
        std::env::remove_var(super::super::FLEET_LOCK_DIR_ENV);
        let brain_root = PathBuf::from("/tmp/some-other-brain-root");
        let lock_dir = super::super::resolve_lock_dir(&brain_root);
        assert_eq!(lock_dir, brain_root.join(".fleet-locks"));
    }

    // -----------------------------------------------------------------------------------------
    // Registry verbs — register / heartbeat / release. `EN.15.C` task 2.
    // -----------------------------------------------------------------------------------------

    fn register_req<'a>(
        agent_name: &'a str,
        repo: &'a str,
        category: Option<&'a str>,
        now_iso: &'a str,
    ) -> RegisterRequest<'a> {
        RegisterRequest {
            agent_name,
            repo,
            lane: "engine-rs",
            roadmap: "coordination-layer-port",
            host: Some("brain-mini"),
            category,
            now_iso,
            now_epoch: 1_787_478_266.0,
            pid: 4242,
        }
    }

    #[test]
    fn register_with_no_category_writes_only_the_registry_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        let outcome = register(
            lock_dir,
            &register_req("engine-rs-1", "engine-rs", None, "2026-09-08T09:00:00Z"),
        )
        .expect("register should succeed");
        assert!(outcome.allowed);

        let claim_path = lock_dir.join("lane-agents").join("agent-engine-rs-1.json");
        assert!(claim_path.exists(), "registry claim must be written");

        // No slot file anywhere under lock_dir root — a light repo consumes no capacity slot.
        let root_json_files: Vec<_> = fs::read_dir(lock_dir)
            .expect("read lock_dir")
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .collect();
        assert!(
            root_json_files.is_empty(),
            "no slot file should be written when category is None"
        );
    }

    #[test]
    fn register_with_category_writes_both_slot_and_registry_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        let outcome = register(
            lock_dir,
            &register_req(
                "engine-rs-1",
                "engine-rs",
                Some("native-build"),
                "2026-09-08T09:00:00Z",
            ),
        )
        .expect("register should succeed");
        assert!(outcome.allowed);

        let claim_path = lock_dir.join("lane-agents").join("agent-engine-rs-1.json");
        assert!(claim_path.exists(), "registry claim must be written");

        let slot_path = lock_dir.join("engine-rs__agent-engine-rs-1.json");
        assert!(slot_path.exists(), "capacity slot must be written");
        let slot: SlotRecord =
            read_typed(&slot_path).expect("slot must parse into its typed shape");
        assert_eq!(slot.category, "native-build");
        assert_eq!(slot.repo, "engine-rs");
        assert_eq!(slot.agent.as_deref(), Some("engine-rs-1"));
    }

    #[test]
    fn repeat_register_for_same_agent_refreshes_without_consuming_a_second_slot() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        for repo in ["one", "two", "three"] {
            let outcome = register(
                lock_dir,
                &register_req(
                    &format!("{repo}-agent"),
                    repo,
                    Some("native-build"),
                    "2026-09-08T09:00:00Z",
                ),
            )
            .expect("register should succeed");
            assert!(outcome.allowed);
        }
        // Repeat register for the SAME repo+agent — must succeed even though the category above
        // now has 4 slots' worth of budget minus 1: three distinct lanes registered, one repeats.
        for _ in 0..3 {
            let outcome = register(
                lock_dir,
                &register_req(
                    "one-agent",
                    "one",
                    Some("native-build"),
                    "2026-09-08T09:10:00Z",
                ),
            )
            .expect("repeat register should succeed");
            assert!(
                outcome.allowed,
                "a repeat register for the same repo+agent must never be refused"
            );
        }

        let entries = category_slot_entries(lock_dir, "native-build");
        assert_eq!(
            entries.len(),
            3,
            "three distinct lanes registered; repeats must not consume extra slots"
        );
    }

    #[test]
    fn heartbeat_restamps_only_heartbeat_field_and_leaves_started_at_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        register(
            lock_dir,
            &register_req("engine-rs-1", "engine-rs", None, "2026-09-08T09:00:00Z"),
        )
        .expect("register should succeed");

        heartbeat(
            lock_dir,
            &HeartbeatRequest {
                agent_name: "engine-rs-1",
                host: Some("brain-mini"),
                now_iso: "2026-09-08T09:30:00Z",
                current_block: Some("EN.15.C"),
                block_started_at: Some("2026-09-08T09:31:00Z"),
            },
        )
        .expect("heartbeat should succeed");

        let claim_path = lock_dir.join("lane-agents").join("agent-engine-rs-1.json");
        let claim: RegistryClaim =
            read_typed(&claim_path).expect("claim must parse into its typed shape");
        assert_eq!(
            claim.started_at, "2026-09-08T09:00:00Z",
            "started_at must never be re-stamped by a heartbeat"
        );
        assert_eq!(claim.heartbeat, "2026-09-08T09:30:00Z");
        assert_eq!(claim.current_block.as_deref(), Some("EN.15.C"));
        assert_eq!(
            claim.block_started_at.as_deref(),
            Some("2026-09-08T09:31:00Z")
        );

        // A follow-up heartbeat that passes no current_block/block_started_at leaves the
        // existing values in place — "when present" means they are not cleared by omission.
        heartbeat(
            lock_dir,
            &HeartbeatRequest {
                agent_name: "engine-rs-1",
                host: Some("brain-mini"),
                now_iso: "2026-09-08T09:45:00Z",
                current_block: None,
                block_started_at: None,
            },
        )
        .expect("second heartbeat should succeed");
        let claim2: RegistryClaim =
            read_typed(&claim_path).expect("claim must parse into its typed shape");
        assert_eq!(claim2.heartbeat, "2026-09-08T09:45:00Z");
        assert_eq!(claim2.started_at, "2026-09-08T09:00:00Z");
        assert_eq!(
            claim2.current_block.as_deref(),
            Some("EN.15.C"),
            "an omitted current_block on a later heartbeat must not clear the existing value"
        );
    }

    #[test]
    fn heartbeat_with_no_existing_claim_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        let err = heartbeat(
            lock_dir,
            &HeartbeatRequest {
                agent_name: "ghost-agent",
                host: None,
                now_iso: "2026-09-08T09:00:00Z",
                current_block: None,
                block_started_at: None,
            },
        )
        .expect_err("heartbeat with no prior claim must be refused");
        assert!(matches!(err, CoordWriteError::Invalid { .. }));
    }

    #[test]
    fn release_removes_claim_and_is_idempotent_on_an_already_absent_claim() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_dir = dir.path();

        register(
            lock_dir,
            &register_req("engine-rs-1", "engine-rs", None, "2026-09-08T09:00:00Z"),
        )
        .expect("register should succeed");
        let claim_path = lock_dir.join("lane-agents").join("agent-engine-rs-1.json");
        assert!(claim_path.exists());

        let removed = release(lock_dir, "engine-rs-1").expect("release should succeed");
        assert!(removed, "an existing claim must report removed: true");
        assert!(
            !claim_path.exists(),
            "claim file must be gone after release"
        );

        let removed_again = release(lock_dir, "engine-rs-1").expect("idempotent release");
        assert!(
            !removed_again,
            "releasing an already-absent claim must report removed: false, not error"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Parity with `fleet_concurrency_check.py register`'s capacity refusal. Shells out to the
    // REAL oracle, the same "skip loudly, never silently pass" pattern
    // `tests/it/coord_parity.rs` (`EN.15.A` task 2) established: never a Rust reimplementation
    // of the oracle asserted against itself.
    // -----------------------------------------------------------------------------------------

    fn find_brain_root(start: &Path) -> Option<PathBuf> {
        let mut dir = Some(start);
        while let Some(d) = dir {
            if d.join("brain.toml").is_file() {
                return Some(d.to_path_buf());
            }
            dir = d.parent();
        }
        None
    }

    fn oracle_script_path(brain_root: &Path) -> PathBuf {
        brain_root
            .join("base-template")
            .join("scripts")
            .join("fleet_concurrency_check.py")
    }

    fn find_oracle_script() -> Option<PathBuf> {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let Some(brain_root) = find_brain_root(manifest_dir) else {
            eprintln!(
                "SKIPPING register capacity parity test: no brain.toml found walking up from {} \
                 (this checkout has no sibling base-template to locate the oracle script in)",
                manifest_dir.display()
            );
            return None;
        };
        let script = oracle_script_path(&brain_root);
        if !script.is_file() {
            eprintln!(
                "SKIPPING register capacity parity test: brain root found at {} but {} does not exist",
                brain_root.display(),
                script.display()
            );
            return None;
        }
        Some(script)
    }

    fn python3_available() -> bool {
        match std::process::Command::new("python3")
            .arg("--version")
            .output()
        {
            Ok(output) => output.status.success(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
            Err(e) => panic!("python3 --version failed unexpectedly: {e}"),
        }
    }

    fn require_parity_environment() -> Option<PathBuf> {
        if !python3_available() {
            eprintln!("SKIPPING register capacity parity test: python3 is not available on PATH");
            return None;
        }
        find_oracle_script()
    }

    fn run_python_register(
        script: &Path,
        lock_dir: &Path,
        repo: &str,
        category: &str,
        agent: &str,
    ) -> std::process::Output {
        std::process::Command::new("python3")
            .arg(script)
            .arg("register")
            .arg("--repo")
            .arg(repo)
            .arg("--category")
            .arg(category)
            .arg("--agent")
            .arg(agent)
            .arg("--lock-dir")
            .arg(lock_dir)
            .output()
            .unwrap_or_else(|e| panic!("failed to spawn python3 register: {e}"))
    }

    /// The headline acceptance criterion: a second `register` on a full `native-build` category
    /// exits 3 with the SAME message `fleet_concurrency_check.py` produces. The cap is
    /// discovered EMPIRICALLY by filling the category through the real oracle until it refuses
    /// — never hardcoded as `4` — and the expected message is captured from a real invocation of
    /// the same oracle against the same lock-dir state, never pasted as a literal string.
    #[test]
    fn second_register_on_full_native_build_category_exits_3_with_same_message_as_python() {
        let Some(script) = require_parity_environment() else {
            return;
        };
        let lock_dir = tempfile::tempdir().expect("tempdir");

        let mut filled = 0usize;
        loop {
            let repo = format!("filler-{filled}");
            let agent = format!("filler-agent-{filled}");
            let output =
                run_python_register(&script, lock_dir.path(), &repo, "native-build", &agent);
            if output.status.success() {
                filled += 1;
                assert!(
                    filled <= 20,
                    "native-build category never filled after 20 registers — cap discovery is broken"
                );
            } else {
                assert_eq!(
                    output.status.code(),
                    Some(3),
                    "a register refusal must exit 3, got: {:?} stderr={}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                );
                break;
            }
        }
        assert!(
            filled >= 1,
            "expected at least one successful register before capacity was hit"
        );

        // Capture the REAL oracle's exact refusal message for one more, distinct repo/agent —
        // against the SAME lock-dir state our Rust register below is about to see.
        let python_refusal = run_python_register(
            &script,
            lock_dir.path(),
            "one-too-many-py",
            "native-build",
            "one-too-many-py-agent",
        );
        assert_eq!(python_refusal.status.code(), Some(3));
        let python_json: serde_json::Value = serde_json::from_slice(&python_refusal.stdout)
            .unwrap_or_else(|e| {
                panic!(
                    "python register refusal output was not valid JSON: {e}\nstdout={}",
                    String::from_utf8_lossy(&python_refusal.stdout)
                )
            });
        assert_eq!(python_json["allowed"].as_bool(), Some(false));
        let python_reason = python_json["reason"]
            .as_str()
            .expect("python refusal JSON must carry a string `reason`")
            .to_string();

        // Our Rust register, against the SAME lock-dir state (the python refusal above wrote
        // nothing, since it was itself refused), for a DIFFERENT new repo/agent — must refuse
        // with the byte-identical message.
        let outcome = register(
            lock_dir.path(),
            &register_req(
                "one-too-many-rs-agent",
                "one-too-many-rs",
                Some("native-build"),
                "2026-09-08T10:00:00Z",
            ),
        )
        .expect("register call itself must not error, only refuse");

        assert!(
            !outcome.allowed,
            "rust register must also refuse once the category is full"
        );
        assert_eq!(
            outcome.reason.as_deref(),
            Some(python_reason.as_str()),
            "rust and python refusal messages must be byte-identical"
        );
    }

    // -----------------------------------------------------------------------------------------
    // Lease verbs — lease / unlease. `EN.15.C` task 3.
    // -----------------------------------------------------------------------------------------

    fn lease_req<'a>(
        repo: &'a str,
        lane: &'a str,
        agent: &'a str,
        now_iso: &'a str,
        window: Option<&'a [String]>,
        lane_blocks: &'a [String],
    ) -> LeaseRequest<'a> {
        LeaseRequest {
            repo,
            lane,
            agent,
            kind: okf_core::LeaseKind::Exclusive,
            scope: Some(okf_core::LeaseScope::Repo),
            host: None,
            now_iso,
            window,
            lane_blocks,
        }
    }

    #[test]
    fn lease_writes_a_record_readable_via_read_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let no_blocks: Vec<String> = Vec::new();
        let req = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T10:00:00Z",
            None,
            &no_blocks,
        );
        lease(dir.path(), &req).expect("lease must succeed");

        let path = lease_path(dir.path(), "engine-rs");
        let record: LeaseRecord = read_typed(&path).expect("lease record must be readable");
        assert_eq!(record.repo, "engine-rs");
        assert_eq!(record.lane, "engine-rs");
        assert_eq!(record.agent, "engine-rs-1");
        assert_eq!(record.acquired_at, "2026-09-08T10:00:00Z");
        assert_eq!(record.heartbeat.as_deref(), Some("2026-09-08T10:00:00Z"));
        assert_eq!(record.kind, okf_core::LeaseKind::Exclusive);
    }

    #[test]
    fn renewed_lease_restamps_heartbeat_only_and_leaves_acquired_at_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let no_blocks: Vec<String> = Vec::new();
        let first = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T10:00:00Z",
            None,
            &no_blocks,
        );
        lease(dir.path(), &first).expect("first lease must succeed");

        let second = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T11:30:00Z",
            None,
            &no_blocks,
        );
        lease(dir.path(), &second).expect("renewal must succeed");

        let record: LeaseRecord =
            read_typed(&lease_path(dir.path(), "engine-rs")).expect("record must be readable");
        assert_eq!(
            record.acquired_at, "2026-09-08T10:00:00Z",
            "acquired_at is set once and must never be re-stamped by a renewal"
        );
        assert_eq!(record.heartbeat.as_deref(), Some("2026-09-08T11:30:00Z"));
    }

    #[test]
    fn lease_whose_window_names_a_block_absent_from_lane_blocks_is_refused_naming_the_block() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane_blocks = vec!["EN.15.A".to_string(), "EN.15.B".to_string()];
        let window = vec!["EN.99.NOPE".to_string()];
        let req = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T10:00:00Z",
            Some(&window),
            &lane_blocks,
        );

        let err = lease(dir.path(), &req).expect_err("a window naming an absent block must refuse");
        match err {
            CoordWriteError::Invalid { reason, .. } => {
                assert!(
                    reason.contains("EN.99.NOPE"),
                    "refusal must name the absent block, got: {reason}"
                );
            }
            other => panic!("expected CoordWriteError::Invalid, got {other:?}"),
        }
        assert!(
            !lease_path(dir.path(), "engine-rs").exists(),
            "a refused lease must write nothing"
        );
    }

    #[test]
    fn lease_whose_window_names_only_blocks_in_lane_blocks_succeeds() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lane_blocks = vec!["EN.15.A".to_string(), "EN.15.C".to_string()];
        let window = vec!["EN.15.C".to_string()];
        let req = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T10:00:00Z",
            Some(&window),
            &lane_blocks,
        );

        lease(dir.path(), &req).expect("a window fully contained in lane_blocks must succeed");
        assert!(lease_path(dir.path(), "engine-rs").exists());
    }

    #[test]
    fn unlease_removes_the_lease_and_is_idempotent_on_an_already_absent_lease() {
        let dir = tempfile::tempdir().expect("tempdir");
        let no_blocks: Vec<String> = Vec::new();
        let req = lease_req(
            "engine-rs",
            "engine-rs",
            "engine-rs-1",
            "2026-09-08T10:00:00Z",
            None,
            &no_blocks,
        );
        lease(dir.path(), &req).expect("lease must succeed");
        assert!(lease_path(dir.path(), "engine-rs").exists());

        let removed = unlease(dir.path(), "engine-rs").expect("unlease must succeed");
        assert!(removed);
        assert!(!lease_path(dir.path(), "engine-rs").exists());

        let removed_again = unlease(dir.path(), "engine-rs").expect("idempotent unlease");
        assert!(
            !removed_again,
            "unleasing an already-absent lease is a no-op, not an error"
        );
    }

    // -----------------------------------------------------------------------------------------
    // The headline criterion: a lease written by this seam is indistinguishable, to the REAL
    // mev, from one written by hand. Drives `mev::set_block_status_as` end to end against a
    // real `.fleet-locks/` tree — the same brain-fixture pattern `close_block.rs`'s `EN.15.B`
    // tests established, reused here rather than re-invented, but with the lease itself written
    // through THIS seam's own `lease`/`unlease` instead of a hand-authored JSON literal.
    // -----------------------------------------------------------------------------------------

    /// A minimal brain root: `brain.toml` naming one repo, plus that repo's own
    /// `planning/state.json` carrying no blocks at all. The quiesce guard is checked BEFORE
    /// `mev::set_block_status_as` ever looks up the block key, so a nonexistent key is
    /// sufficient to prove the refusal (or its absence) without depending on — or risking a
    /// write to — any real block record.
    fn brain_fixture(repo: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo_dir = dir.path().join(repo);
        fs::create_dir_all(repo_dir.join("planning")).expect("mkdir");
        fs::write(
            dir.path().join("brain.toml"),
            format!("[[repos]]\nslug = \"{repo}\"\nrepo_path = \"{repo}\"\n"),
        )
        .expect("write brain.toml");
        fs::write(
            repo_dir.join("planning").join("state.json"),
            format!(
                r#"{{ "repo": "{repo}", "kind": "project", "updated": "2026-08-20",
  "focus": {{ "now": [], "next": [], "blocked": [] }},
  "tracks": [{{ "title": "P1", "blocks": [] }}] }}"#
            ),
        )
        .expect("write state.json");
        (dir, repo_dir)
    }

    #[test]
    fn mev_check_quiesce_refuses_set_block_status_write_after_a_lease_written_by_this_seam_and_allows_it_after_release(
    ) {
        let (dir, repo_dir) = brain_fixture("engine-rs");
        let root = dir.path();
        // `mev::set_block_status_as`'s own `lock_dir: None` resolves to `<root>/.fleet-locks`
        // (`mev::brain::lease::resolve_lock_dir`'s default) — this seam's `lease`/`unlease`
        // take an already-resolved lock dir, so the two must agree on the SAME path here.
        let lock_dir = root.join(".fleet-locks");
        let no_blocks: Vec<String> = Vec::new();

        // No lease yet: clear to proceed (block key does not exist, so this reports
        // E_BLOCK_NOT_FOUND rather than mutating anything — proof the guard itself is clear,
        // not proof the whole call succeeded).
        let clear_before = mev::set_block_status_as(
            root,
            "engine-rs:EN.99.NOPE",
            "closed",
            true,
            None,
            Some("engine-rs-holder"),
            None,
            &repo_dir,
        );
        assert!(
            clear_before.is_ok(),
            "with no lease held, set_block_status_as must not be refused by the quiesce guard: {:?}",
            clear_before.err()
        );

        // Write the lease through THIS seam.
        let req = lease_req(
            "engine-rs",
            "test-lane",
            "engine-rs-holder",
            "2026-09-08T10:00:00Z",
            None,
            &no_blocks,
        );
        lease(&lock_dir, &req).expect("lease must succeed");

        // A DIFFERENT identity is refused with E_QUIESCE_LEASE_HELD.
        let refused = mev::set_block_status_as(
            root,
            "engine-rs:EN.99.NOPE",
            "closed",
            true,
            None,
            Some("engine-rs-someone-else"),
            None,
            &repo_dir,
        );
        let err =
            refused.expect_err("a different identity must be refused while the lease is live");
        assert!(
            err.to_string().contains(mev::E_QUIESCE_LEASE_HELD),
            "expected E_QUIESCE_LEASE_HELD in the refusal, got: {err}"
        );

        // The HOLDER's own write passes — self-exemption. Both directions asserted, so this
        // cannot pass against a disabled guard.
        let allowed_for_holder = mev::set_block_status_as(
            root,
            "engine-rs:EN.99.NOPE",
            "closed",
            true,
            None,
            Some("engine-rs-holder"),
            None,
            &repo_dir,
        );
        assert!(
            allowed_for_holder.is_ok(),
            "the lease's own holder must self-exempt: {:?}",
            allowed_for_holder.err()
        );

        // Release through THIS seam, then re-check: a caller identified as the FORMER
        // non-holder now proceeds too.
        let removed = unlease(&lock_dir, "engine-rs").expect("unlease must succeed");
        assert!(removed);

        let clear_after = mev::set_block_status_as(
            root,
            "engine-rs:EN.99.NOPE",
            "closed",
            true,
            None,
            Some("engine-rs-someone-else"),
            None,
            &repo_dir,
        );
        assert!(
            clear_after.is_ok(),
            "after release, the write must no longer be refused by the quiesce guard: {:?}",
            clear_after.err()
        );
    }
}
