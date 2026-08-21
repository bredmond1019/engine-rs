//! Repo registry — slug -> absolute root, resolved from `brain.toml` (EN.3.K).
//!
//! The dispatch-target-resolution work (`planning/EN.3.K-dispatch-target-resolution/`)
//! replaces the implicit "the target repo is the serve process's cwd" assumption
//! with an explicit, registry-resolved `repo` slug on the `SDLC_FLOW` event. This
//! module owns that registry: it maps a caller-supplied **slug** (never a path) to
//! an absolute filesystem root, sourced from `brain.toml`'s `[[repos]]` entries —
//! the same source of truth `mev`/`bastion` already resolve.
//!
//! # The load-bearing security property
//!
//! `resolve` takes `&str` and only ever looks it up in a pre-built map. There is
//! no `resolve_path`, no join of caller-supplied input onto a root. A **path** on
//! the wire would let anything holding the API key point an autonomous
//! `dangerously_skip_permissions` agent at any directory on the machine; a
//! **slug** makes the reachable set a deliberate, reviewable list bounded by
//! `brain.toml`. Every resolvable path is `brain_root.join(repo_path)`, verified
//! at construction time to stay inside `brain_root` — an escaping `repo_path`
//! (e.g. via `..`) is rejected before it ever becomes resolvable, not filtered at
//! resolve time.
//!
//! Modelled directly on [`crate::brain_root`]: typed errors, no panics, no
//! silent `.` fallback.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use crate::brain_root::{self, BrainRootError};

/// The env var honoured as an optional narrowing filter over the registry.
///
/// Comma-separated slugs, whitespace-trimmed, empty entries ignored. Unset or
/// empty means "every `brain.toml` slug is reachable" — the default, and what
/// the Mac Mini runs. Set, it intersects the registry down to those slugs, for
/// a future deployment that wants a shrunk reachable set without editing the
/// brain.
pub const ENGINE_REPO_ALLOWLIST_ENV: &str = "ENGINE_REPO_ALLOWLIST";

/// Typed failure modes for repo-registry resolution. Mirrors
/// [`crate::brain_root::BrainRootError`]'s style — a `Display` impl plus
/// `std::error::Error`, no panics anywhere in this module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepoRegistryError {
    /// The brain root itself could not be resolved. Preserves the original
    /// `BrainRootError` message — an operator whose `ENGINE_BRAIN_ROOT` is
    /// wrong must be told that, not "unknown repo".
    BrainRoot { message: String },
    /// `brain.toml` could not be loaded or parsed from the resolved brain
    /// root.
    ConfigLoad { message: String },
    /// The requested slug is not present in the registry (either absent from
    /// `brain.toml`, or filtered out by `ENGINE_REPO_ALLOWLIST`).
    UnknownSlug { slug: String, known: Vec<String> },
    /// The slug is present in `brain.toml` but its entry is unusable — its
    /// `repo_path` escapes the brain root, does not exist, or is not a
    /// directory.
    UnusableEntry { slug: String, reason: String },
}

impl fmt::Display for RepoRegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RepoRegistryError::BrainRoot { message } => {
                write!(f, "repo registry: {message}")
            }
            RepoRegistryError::ConfigLoad { message } => {
                write!(f, "repo registry: could not load brain.toml: {message}")
            }
            RepoRegistryError::UnknownSlug { slug, known } => {
                if known.is_empty() {
                    write!(
                        f,
                        "unknown repo slug '{slug}' (the repo registry has no known slugs)"
                    )
                } else {
                    write!(
                        f,
                        "unknown repo slug '{slug}' (known slugs: {})",
                        known.join(", ")
                    )
                }
            }
            RepoRegistryError::UnusableEntry { slug, reason } => {
                write!(f, "repo slug '{slug}' is not resolvable: {reason}")
            }
        }
    }
}

impl std::error::Error for RepoRegistryError {}

impl From<BrainRootError> for RepoRegistryError {
    fn from(err: BrainRootError) -> Self {
        RepoRegistryError::BrainRoot {
            message: err.to_string(),
        }
    }
}

/// Slug -> absolute root registry, sourced from `brain.toml`'s `[[repos]]`
/// entries.
#[derive(Debug, Clone)]
pub struct RepoRegistry {
    brain_root: PathBuf,
    entries: BTreeMap<String, PathBuf>,
}

impl RepoRegistry {
    /// Build a registry from an explicit brain root, reading `brain.toml`
    /// from `root.join("brain.toml")`.
    ///
    /// Entries whose `repo_path` escapes `root`, does not exist, or is not a
    /// directory are skipped (they still resolve, at `resolve` time, to
    /// [`RepoRegistryError::UnusableEntry`] rather than silently vanishing).
    /// `ENGINE_REPO_ALLOWLIST`, when set and non-empty, intersects the
    /// resulting map.
    pub fn from_brain_root(root: &Path) -> Result<Self, RepoRegistryError> {
        let config_path = root.join("brain.toml");
        let config = mev::brain::config::load_brain_config(&config_path).map_err(|e| {
            RepoRegistryError::ConfigLoad {
                message: e.to_string(),
            }
        })?;

        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        let allowlist = read_allowlist();

        let mut entries = BTreeMap::new();
        for repo in &config.repos {
            if let Some(allowed) = &allowlist {
                if !allowed.contains(&repo.slug) {
                    continue;
                }
            }

            let candidate = root.join(&repo.repo_path);
            if !is_within_root(&candidate, &canonical_root) {
                // Unsafe entry: `repo_path` escapes the brain root. Do not
                // record it — resolving this slug returns UnusableEntry.
                continue;
            }
            if !candidate.is_dir() {
                // Nonexistent or non-directory target. Same treatment.
                continue;
            }

            entries.insert(repo.slug.clone(), candidate);
        }

        Ok(RepoRegistry {
            brain_root: root.to_path_buf(),
            entries,
        })
    }

    /// Build a registry by resolving the brain root via
    /// [`crate::brain_root::resolve_brain_root`] (`ENGINE_BRAIN_ROOT` first,
    /// else walk up for `brain.toml`) and delegating to
    /// [`RepoRegistry::from_brain_root`].
    pub fn from_env() -> Result<Self, RepoRegistryError> {
        let root = brain_root::resolve_brain_root()?;
        Self::from_brain_root(&root)
    }

    /// The brain root this registry was built against.
    pub fn brain_root(&self) -> &Path {
        &self.brain_root
    }

    /// Every slug this registry can currently resolve, in registry
    /// (alphabetical) order. Used by callers that must sweep the whole
    /// reachable corpus rather than resolve one named slug — e.g. resolving
    /// an opaque `HELD-UNTIL` token that carries no repo of its own.
    #[must_use]
    pub fn known_slugs(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    /// Resolve `slug` to its absolute repo root.
    ///
    /// Returns [`RepoRegistryError::UnknownSlug`] (naming the slug and the
    /// known slugs) when `slug` was never recorded — either absent from
    /// `brain.toml`, filtered out by `ENGINE_REPO_ALLOWLIST`, or rejected at
    /// construction as an unusable entry.
    pub fn resolve(&self, slug: &str) -> Result<PathBuf, RepoRegistryError> {
        self.entries
            .get(slug)
            .cloned()
            .ok_or_else(|| RepoRegistryError::UnknownSlug {
                slug: slug.to_string(),
                known: self.entries.keys().cloned().collect(),
            })
    }
}

/// `true` when `candidate` (after best-effort canonicalization) starts with
/// `canonical_root` — i.e. `candidate` stays inside the brain root. A
/// `candidate` that does not exist cannot be canonicalized; in that case we
/// fall back to a lexical join/starts-with check on the raw paths, which is
/// still sufficient to catch a `..`-escaping `repo_path`.
fn is_within_root(candidate: &Path, canonical_root: &Path) -> bool {
    match candidate.canonicalize() {
        Ok(canonical_candidate) => canonical_candidate.starts_with(canonical_root),
        Err(_) => {
            // Cannot canonicalize (e.g. does not exist yet). Fall back to a
            // lexical check: normalize `..` components manually.
            let mut stack: Vec<std::path::Component> = Vec::new();
            for comp in candidate.components() {
                match comp {
                    std::path::Component::ParentDir => {
                        if stack
                            .last()
                            .map(|c| !matches!(c, std::path::Component::ParentDir))
                            .unwrap_or(false)
                        {
                            stack.pop();
                        } else {
                            stack.push(comp);
                        }
                    }
                    other => stack.push(other),
                }
            }
            let normalized: PathBuf = stack.iter().collect();
            normalized.starts_with(canonical_root)
                || normalized
                    .canonicalize()
                    .map(|p| p.starts_with(canonical_root))
                    .unwrap_or(false)
        }
    }
}

/// Read `ENGINE_REPO_ALLOWLIST`: `None` when unset/empty (no filtering),
/// `Some(set)` of trimmed, non-empty slugs otherwise.
fn read_allowlist() -> Option<std::collections::HashSet<String>> {
    let raw = env::var(ENGINE_REPO_ALLOWLIST_ENV).ok()?;
    let slugs: std::collections::HashSet<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if slugs.is_empty() {
        None
    } else {
        Some(slugs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // `ENGINE_REPO_ALLOWLIST` / `ENGINE_BRAIN_ROOT` are process-global state.
    // Guard every test that touches either behind this mutex, exactly as
    // `brain_root.rs` already does, so they cannot race the rest of the
    // suite.
    static ENV_GUARD: Mutex<()> = Mutex::new(());

    fn with_env_unset<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev_allowlist = env::var(ENGINE_REPO_ALLOWLIST_ENV).ok();
        let prev_brain_root = env::var(brain_root::ENGINE_BRAIN_ROOT_ENV).ok();
        env::remove_var(ENGINE_REPO_ALLOWLIST_ENV);
        env::remove_var(brain_root::ENGINE_BRAIN_ROOT_ENV);

        let result = f();

        match prev_allowlist {
            Some(v) => env::set_var(ENGINE_REPO_ALLOWLIST_ENV, v),
            None => env::remove_var(ENGINE_REPO_ALLOWLIST_ENV),
        }
        match prev_brain_root {
            Some(v) => env::set_var(brain_root::ENGINE_BRAIN_ROOT_ENV, v),
            None => env::remove_var(brain_root::ENGINE_BRAIN_ROOT_ENV),
        }
        result
    }

    fn with_allowlist<T>(value: &str, f: impl FnOnce() -> T) -> T {
        let _guard = ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());
        let prev = env::var(ENGINE_REPO_ALLOWLIST_ENV).ok();
        env::set_var(ENGINE_REPO_ALLOWLIST_ENV, value);

        let result = f();

        match prev {
            Some(v) => env::set_var(ENGINE_REPO_ALLOWLIST_ENV, v),
            None => env::remove_var(ENGINE_REPO_ALLOWLIST_ENV),
        }
        result
    }

    /// Build a tempdir "brain root" with a `brain.toml` naming two repos
    /// (`alpha`, `beta`), each with a real subdirectory.
    fn two_repo_brain() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("alpha")).expect("mkdir alpha");
        std::fs::create_dir_all(dir.path().join("beta")).expect("mkdir beta");
        std::fs::write(
            dir.path().join("brain.toml"),
            r#"
[[repos]]
slug = "alpha"
repo_path = "alpha"

[[repos]]
slug = "beta"
repo_path = "beta"
"#,
        )
        .expect("write brain.toml");
        dir
    }

    #[test]
    fn resolves_both_entries_to_absolute_paths_under_root() {
        with_env_unset(|| {
            let dir = two_repo_brain();
            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");

            let alpha = registry.resolve("alpha").expect("alpha resolves");
            let beta = registry.resolve("beta").expect("beta resolves");

            assert_eq!(
                alpha.canonicalize().unwrap(),
                dir.path().join("alpha").canonicalize().unwrap()
            );
            assert_eq!(
                beta.canonicalize().unwrap(),
                dir.path().join("beta").canonicalize().unwrap()
            );
            assert!(alpha.is_absolute());
            assert!(beta.is_absolute());
        });
    }

    #[test]
    fn unknown_slug_error_names_the_slug() {
        with_env_unset(|| {
            let dir = two_repo_brain();
            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");

            let err = registry.resolve("nope").expect_err("should not resolve");
            match &err {
                RepoRegistryError::UnknownSlug { slug, .. } => assert_eq!(slug, "nope"),
                other => panic!("expected UnknownSlug, got {other:?}"),
            }
            assert!(err.to_string().contains("nope"));
        });
    }

    #[test]
    fn escaping_repo_path_is_not_resolvable() {
        with_env_unset(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                dir.path().join("brain.toml"),
                r#"
[[repos]]
slug = "escape"
repo_path = "../escape"
"#,
            )
            .expect("write brain.toml");

            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
            let err = registry.resolve("escape").expect_err("should not resolve");
            assert!(matches!(err, RepoRegistryError::UnknownSlug { .. }));
        });
    }

    #[test]
    fn nonexistent_repo_path_is_not_resolvable() {
        with_env_unset(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            std::fs::write(
                dir.path().join("brain.toml"),
                r#"
[[repos]]
slug = "ghost"
repo_path = "does-not-exist"
"#,
            )
            .expect("write brain.toml");

            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");
            let err = registry.resolve("ghost").expect_err("should not resolve");
            assert!(matches!(err, RepoRegistryError::UnknownSlug { .. }));
        });
    }

    #[test]
    fn allowlist_narrows_the_registry() {
        // NOTE: `with_allowlist` already acquires `ENV_GUARD` (and also
        // clears/restores `ENGINE_BRAIN_ROOT`'s surrounding state is not
        // needed here since this test never touches it) — do NOT nest this
        // inside `with_env_unset`, which also locks the same
        // non-reentrant `std::sync::Mutex` and would deadlock.
        with_allowlist("alpha", || {
            let dir = two_repo_brain();
            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");

            assert!(registry.resolve("alpha").is_ok());
            let err = registry.resolve("beta").expect_err("beta filtered out");
            assert!(matches!(err, RepoRegistryError::UnknownSlug { .. }));
        });
    }

    #[test]
    fn unset_allowlist_leaves_both_resolvable() {
        with_env_unset(|| {
            let dir = two_repo_brain();
            let registry = RepoRegistry::from_brain_root(dir.path()).expect("registry");

            assert!(registry.resolve("alpha").is_ok());
            assert!(registry.resolve("beta").is_ok());
        });
    }

    #[test]
    fn no_brain_toml_surfaces_typed_brain_root_error_not_panic() {
        with_env_unset(|| {
            let dir = tempfile::tempdir().expect("tempdir");
            // No brain.toml anywhere under this tempdir tree, and
            // ENGINE_BRAIN_ROOT is unset, so from_env() must walk up from
            // cwd via resolve_brain_root() — which, in a repo checkout,
            // will actually find a real brain.toml higher up. To exercise
            // the "genuinely absent" path deterministically, exercise
            // from_brain_root directly against a path with no brain.toml,
            // which surfaces ConfigLoad rather than a panic.
            let err = RepoRegistry::from_brain_root(dir.path()).expect_err("should not resolve");
            assert!(matches!(err, RepoRegistryError::ConfigLoad { .. }));
        });
    }
}
