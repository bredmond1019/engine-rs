//! Brain-root / target-corpus resolution (EN.7.A).
//!
//! `engine-core` has no notion of a target corpus today — the workflows that
//! materialize `.md` documents into the Brain (`agentic-portfolio`) need to
//! know where that corpus root lives on disk. This module owns that
//! resolution, following the repo's env-var naming convention
//! (`ENGINE_EVENTS_URL`, `ENGINE_RUN_MAX_COST_USD`): the new knob is
//! `ENGINE_BRAIN_ROOT`.
//!
//! Precedence:
//! 1. `ENGINE_BRAIN_ROOT` when set and non-empty — must point at an existing
//!    directory, or resolution fails with a typed error (never a silent
//!    fallback).
//! 2. [`mev::brain::config::find_brain_root`], walking up from the process
//!    cwd (or an explicit start directory via [`resolve_brain_root_from`])
//!    looking for `brain.toml`.
//!
//! Every failure path returns [`BrainRootError`] — this module never panics
//! and never silently defaults to `.`.

use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

/// The env var honoured as the first-precedence brain-root override.
pub const ENGINE_BRAIN_ROOT_ENV: &str = "ENGINE_BRAIN_ROOT";

/// Typed failure modes for brain-root resolution. Mirrors the repo's
/// existing error style (see `NodeError` in `crate::node`) — a `Display`
/// impl plus `std::error::Error`, no panics anywhere in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrainRootError {
    /// `ENGINE_BRAIN_ROOT` was set but the path it names does not exist or
    /// is not a directory.
    EnvPathInvalid { path: PathBuf },
    /// No `brain.toml` was found walking up from the start directory.
    NotFound { start: PathBuf },
    /// The current working directory could not be read.
    CwdUnreadable { message: String },
}

impl fmt::Display for BrainRootError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BrainRootError::EnvPathInvalid { path } => write!(
                f,
                "ENGINE_BRAIN_ROOT is set to '{}' but that path does not exist or is not a directory",
                path.display()
            ),
            BrainRootError::NotFound { start } => write!(
                f,
                "no brain.toml found walking up from '{}' (set ENGINE_BRAIN_ROOT to override)",
                start.display()
            ),
            BrainRootError::CwdUnreadable { message } => {
                write!(f, "could not read the current working directory: {message}")
            }
        }
    }
}

impl std::error::Error for BrainRootError {}

/// Resolve the brain root from the process's current working directory.
///
/// Honours `ENGINE_BRAIN_ROOT` first; falls back to
/// `mev::brain::config::find_brain_root` walking up from cwd. Returns a
/// typed [`BrainRootError`] — never a panic, never a silent `.` default.
pub fn resolve_brain_root() -> Result<PathBuf, BrainRootError> {
    if let Some(root) = env_override()? {
        return Ok(root);
    }

    let cwd = env::current_dir().map_err(|e| BrainRootError::CwdUnreadable {
        message: e.to_string(),
    })?;
    resolve_from_start(&cwd)
}

/// Resolve the brain root as if the process were running from `start`,
/// still honouring `ENGINE_BRAIN_ROOT` first. Lets tests and callers resolve
/// deterministically without touching process-global cwd state.
pub fn resolve_brain_root_from(start: &Path) -> Result<PathBuf, BrainRootError> {
    if let Some(root) = env_override()? {
        return Ok(root);
    }
    resolve_from_start(start)
}

/// Read and validate `ENGINE_BRAIN_ROOT`, if set and non-empty.
fn env_override() -> Result<Option<PathBuf>, BrainRootError> {
    match env::var(ENGINE_BRAIN_ROOT_ENV) {
        Ok(raw) if !raw.trim().is_empty() => {
            let path = PathBuf::from(raw);
            if path.is_dir() {
                Ok(Some(path))
            } else {
                Err(BrainRootError::EnvPathInvalid { path })
            }
        }
        _ => Ok(None),
    }
}

/// Walk up from `start` looking for `brain.toml` via `mev`'s resolver,
/// mapping its `ConfigError` into our typed [`BrainRootError`].
fn resolve_from_start(start: &Path) -> Result<PathBuf, BrainRootError> {
    mev::brain::config::find_brain_root(start).map_err(|_| BrainRootError::NotFound {
        start: start.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `ENGINE_BRAIN_ROOT` is process-global state. Guard every test that
    // touches it behind this mutex so they cannot race the rest of the
    // suite (or each other, if `cargo test` ever runs this module's tests
    // in parallel threads within one process).
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    // `resolve_brain_root_from` still checks `ENGINE_BRAIN_ROOT` first (env
    // override always wins), and env vars are process-global — so even
    // tests that never *set* the var must hold the guard and confirm it is
    // unset for their duration, or a concurrently-running env-setting test
    // in another thread can leak through and flip their expectation.
    fn with_env_unset<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = env::var(ENGINE_BRAIN_ROOT_ENV).ok();
        env::remove_var(ENGINE_BRAIN_ROOT_ENV);

        let result = f();

        if let Some(v) = previous {
            env::set_var(ENGINE_BRAIN_ROOT_ENV, v);
        }
        result
    }

    #[test]
    fn resolves_from_explicit_start_dir_with_brain_toml() {
        with_env_unset(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(dir.path().join("brain.toml"), "").expect("write brain.toml");

            let resolved = resolve_brain_root_from(dir.path()).expect("should resolve");
            assert_eq!(
                resolved.canonicalize().unwrap(),
                dir.path().canonicalize().unwrap()
            );
        });
    }

    #[test]
    fn returns_typed_error_when_no_brain_toml_up_tree() {
        with_env_unset(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            // No brain.toml anywhere under this tempdir tree.
            let err = resolve_brain_root_from(dir.path()).expect_err("should not resolve");
            match err {
                BrainRootError::NotFound { .. } => {}
                other => panic!("expected NotFound, got {other:?}"),
            }
        });
    }

    #[test]
    fn env_override_pointing_at_nonexistent_path_is_typed_error() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = env::var(ENGINE_BRAIN_ROOT_ENV).ok();

        env::set_var(ENGINE_BRAIN_ROOT_ENV, "/definitely/not/a/real/path/en-7-a");
        let err = resolve_brain_root().expect_err("should not resolve");
        match err {
            BrainRootError::EnvPathInvalid { .. } => {}
            other => panic!("expected EnvPathInvalid, got {other:?}"),
        }

        match previous {
            Some(v) => env::set_var(ENGINE_BRAIN_ROOT_ENV, v),
            None => env::remove_var(ENGINE_BRAIN_ROOT_ENV),
        }
    }

    #[test]
    fn env_override_pointing_at_real_dir_wins_over_walk_up() {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let previous = env::var(ENGINE_BRAIN_ROOT_ENV).ok();

        let dir = tempfile::tempdir().expect("tempdir");
        env::set_var(ENGINE_BRAIN_ROOT_ENV, dir.path());
        let resolved = resolve_brain_root().expect("should resolve via env override");
        assert_eq!(
            resolved.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap()
        );

        match previous {
            Some(v) => env::set_var(ENGINE_BRAIN_ROOT_ENV, v),
            None => env::remove_var(ENGINE_BRAIN_ROOT_ENV),
        }
    }
}
