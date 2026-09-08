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

use okf_core::{Coord, HeartbeatValue};

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

#[cfg(test)]
mod tests {
    use super::*;
    use okf_core::{LeaseRecord, RegistryClaim};

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
}
