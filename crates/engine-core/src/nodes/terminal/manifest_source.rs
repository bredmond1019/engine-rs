//! `manifest_source` (`EN.9.F` task 1) — a runtime override for the detect
//! manifest, layered around `term_core::detect::CLAUDE_MANIFEST_TOML`.
//!
//! # Why this exists
//!
//! Today the production manifest is `include_str!`'d at compile time
//! (`term-core/src/detect/mod.rs:41`). The claude manifest matches three
//! literal UI strings, and Claude Code ships frequently — the one-word fix
//! for a reworded string ("Do you want to proceed?") otherwise needs a
//! rebuild AND a redeploy of the installed binary. [`ManifestSource`] adds a
//! path that is re-read and re-compiled on change, so an on-disk edit takes
//! effect on the NEXT [`ManifestSource::resolve`] call with no rebuild and
//! no process restart.
//!
//! # Config, not a hardcoded location
//!
//! The override path is resolved from [`MANIFEST_OVERRIDE_PATH_ENV`],
//! following this repo's existing env-var convention for process-level
//! config (`ENGINE_BRAIN_ROOT` in `crate::brain_root`, `ENGINE_REPO_ALLOWLIST`
//! in `crate::repo_registry`). Unset (or empty) is the behavior-stable
//! default: [`ManifestSource::resolve`] returns the embedded const, byte-
//! identical to reading `CLAUDE_MANIFEST_TOML` directly today (CLAUDE.md
//! standing rule 6 — adding the knob must not change what an existing run
//! does).
//!
//! # Caching
//!
//! The compiled manifest is cached between calls — recompiling a manifest
//! (parsing TOML, compiling every rule's regexes) on every single capture is
//! the cost this cache exists to avoid. The embedded manifest is fixed for
//! the process lifetime and is compiled at most once. The override manifest
//! is re-read only when the override file's mtime changes; an unchanged
//! mtime returns the cached compile.
//!
//! # Failure mode: a malformed override must not take down detection
//!
//! An override file that fails to read or fails to parse/compile does NOT
//! propagate an error to the caller. [`ManifestSource::resolve`] instead:
//!
//! 1. Logs loudly (`eprintln!` — this workspace carries no `tracing`/`log`
//!    dependency; see `crate::workflows::sdlc_flow::log_noop_commit`'s doc
//!    comment for the same precedent).
//! 2. Keeps serving the last-good compiled override, if one has ever been
//!    loaded successfully from this same path.
//! 3. Only when there is no last-good override yet does it fall back to the
//!    embedded manifest — loudly, via the same log line, so the fallback is
//!    never silent. "Never silently fall back" is about visibility, not
//!    about refusing to serve *something*: detection must keep running.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use sha2::{Digest as _, Sha256};
use term_core::detect::manifest::{parse_manifest, CompiledManifest, ManifestError};
use term_core::detect::CLAUDE_MANIFEST_TOML;

/// The env var honoured as the runtime override path for the detect
/// manifest. Unset or empty resolves to `term_core::detect::CLAUDE_MANIFEST_TOML`.
pub const MANIFEST_OVERRIDE_PATH_ENV: &str = "ENGINE_TERMINAL_MANIFEST_OVERRIDE";

/// Where a [`ResolvedManifest`] actually came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestOrigin {
    /// `term_core::detect::CLAUDE_MANIFEST_TOML`, compiled at compile time.
    Embedded,
    /// A runtime override file, resolved from [`MANIFEST_OVERRIDE_PATH_ENV`].
    Override(PathBuf),
}

/// A compiled manifest plus the identity an operator needs to answer "is the
/// manifest I think is deployed the one actually running?" — its
/// [`ManifestOrigin`] and the hex SHA-256 digest of the raw TOML source that
/// compiled it. `EN.9.F` task 2's no-match alarm names both.
#[derive(Clone)]
pub struct ResolvedManifest {
    pub manifest: Arc<CompiledManifest>,
    /// Hex-encoded SHA-256 of the raw TOML source (not of the compiled
    /// form) — this is what lets an operator diff "digest the alarm named"
    /// against "digest of the file I just edited".
    pub digest: String,
    pub origin: ManifestOrigin,
}

/// Hex-encoded SHA-256 of `s`.
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn compile_source(toml_src: &str) -> Result<Arc<CompiledManifest>, ManifestError> {
    let manifest = parse_manifest(toml_src)?;
    let compiled = manifest.compile()?;
    Ok(Arc::new(compiled))
}

/// Cached override state: the last path/mtime this source successfully
/// compiled from, plus the [`ResolvedManifest`] that compile produced.
struct OverrideCache {
    path: PathBuf,
    mtime: SystemTime,
    last_good: ResolvedManifest,
}

#[derive(Default)]
struct Cache {
    /// Compiled at most once per process — the embedded const never
    /// changes at runtime.
    embedded: Option<ResolvedManifest>,
    override_state: Option<OverrideCache>,
}

/// Resolves the detect manifest at runtime: an override file when one is
/// configured and valid, the embedded const otherwise. See the module doc
/// for the full precedence and failure-mode rules.
pub struct ManifestSource {
    override_path: Option<PathBuf>,
    cache: Mutex<Cache>,
}

impl ManifestSource {
    /// Build a source with an explicit override path (or `None` for "no
    /// override" — the behavior-stable default). Prefer [`Self::from_env`]
    /// in production call sites; this constructor exists for tests and for
    /// callers that resolve the path from something other than the env var.
    pub fn new(override_path: Option<PathBuf>) -> Self {
        Self {
            override_path,
            cache: Mutex::new(Cache::default()),
        }
    }

    /// Build a source with the override path resolved from
    /// [`MANIFEST_OVERRIDE_PATH_ENV`]. An unset or empty (whitespace-only)
    /// value is treated as "no override configured".
    pub fn from_env() -> Self {
        let override_path = std::env::var(MANIFEST_OVERRIDE_PATH_ENV)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from);
        Self::new(override_path)
    }

    /// Resolve the currently-active manifest. Re-reads and re-compiles the
    /// override file when its mtime has changed since the last resolve;
    /// otherwise returns the cached compile. See the module doc's "failure
    /// mode" section for what happens when the override is missing,
    /// unreadable, or malformed.
    pub fn resolve(&self) -> ResolvedManifest {
        let mut cache = self.cache.lock().expect("manifest source cache poisoned");

        let Some(path) = self.override_path.clone() else {
            return Self::embedded(&mut cache);
        };

        match fs::metadata(&path).and_then(|m| m.modified()) {
            Ok(mtime) => {
                if let Some(oc) = &cache.override_state {
                    if oc.path == path && oc.mtime == mtime {
                        return oc.last_good.clone();
                    }
                }
                match fs::read_to_string(&path) {
                    Ok(src) => match compile_source(&src) {
                        Ok(manifest) => {
                            let resolved = ResolvedManifest {
                                manifest,
                                digest: sha256_hex(&src),
                                origin: ManifestOrigin::Override(path.clone()),
                            };
                            cache.override_state = Some(OverrideCache {
                                path,
                                mtime,
                                last_good: resolved.clone(),
                            });
                            resolved
                        }
                        Err(err) => {
                            eprintln!(
                                "terminal::manifest_source: WARNING override manifest at \
                                 '{}' failed to compile ({err}); {}",
                                path.display(),
                                Self::fallback_reason(&cache, &path)
                            );
                            Self::fallback_after_bad_override(&mut cache, &path)
                        }
                    },
                    Err(err) => {
                        eprintln!(
                            "terminal::manifest_source: WARNING override manifest at \
                             '{}' could not be read ({err}); {}",
                            path.display(),
                            Self::fallback_reason(&cache, &path)
                        );
                        Self::fallback_after_bad_override(&mut cache, &path)
                    }
                }
            }
            Err(err) => {
                eprintln!(
                    "terminal::manifest_source: WARNING override manifest path \
                     '{}' has no metadata ({err}); {}",
                    path.display(),
                    Self::fallback_reason(&cache, &path)
                );
                Self::fallback_after_bad_override(&mut cache, &path)
            }
        }
    }

    /// Human-readable clause describing which fallback branch a bad-override
    /// log line is about to take — factored out so both branches log the
    /// same wording before [`Self::fallback_after_bad_override`] decides.
    fn fallback_reason(cache: &Cache, path: &Path) -> &'static str {
        match &cache.override_state {
            Some(oc) if oc.path == path => "keeping the last-good compiled override manifest",
            _ => "no last-good override manifest yet; falling back to the embedded manifest",
        }
    }

    /// On a bad override read/compile: keep serving the last-good compiled
    /// override for this same path if one exists, otherwise fall back to
    /// the embedded manifest. Both branches were already announced by the
    /// caller's `eprintln!` — this never falls back *silently*.
    fn fallback_after_bad_override(cache: &mut Cache, path: &Path) -> ResolvedManifest {
        if let Some(oc) = &cache.override_state {
            if oc.path == path {
                return oc.last_good.clone();
            }
        }
        Self::embedded(cache)
    }

    fn embedded(cache: &mut Cache) -> ResolvedManifest {
        cache
            .embedded
            .get_or_insert_with(|| ResolvedManifest {
                manifest: compile_source(CLAUDE_MANIFEST_TOML)
                    .expect("CLAUDE_MANIFEST_TOML is a fixed, valid manifest"),
                digest: sha256_hex(CLAUDE_MANIFEST_TOML),
                origin: ManifestOrigin::Embedded,
            })
            .clone()
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    fn valid_manifest_toml(name: &str) -> String {
        format!(
            r#"
name = "{name}"

[[rules]]
state = "working"
gate = {{ contains = "spinner" }}
"#
        )
    }

    fn write_tmp(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let path = dir.join(name);
        let mut f = fs::File::create(&path).expect("create tmp manifest file");
        f.write_all(contents.as_bytes())
            .expect("write tmp manifest file");
        path
    }

    /// Force the mtime to visibly change even on filesystems with coarse
    /// mtime resolution (some macOS/CI filesystems round to ~1s).
    fn touch_later(path: &Path, contents: &str) {
        thread::sleep(Duration::from_millis(1100));
        let mut f = fs::File::create(path).expect("rewrite tmp manifest file");
        f.write_all(contents.as_bytes())
            .expect("rewrite tmp manifest file");
    }

    #[test]
    fn no_override_resolves_the_embedded_const() {
        let source = ManifestSource::new(None);
        let resolved = source.resolve();
        assert_eq!(resolved.origin, ManifestOrigin::Embedded);
        assert_eq!(resolved.digest, sha256_hex(CLAUDE_MANIFEST_TOML));
        assert_eq!(resolved.manifest.name, "claude");
    }

    #[test]
    fn no_override_is_byte_identical_to_reading_the_embedded_const_directly() {
        let source = ManifestSource::new(None);
        let resolved = source.resolve();
        let direct = compile_source(CLAUDE_MANIFEST_TOML).expect("embedded const compiles");
        assert_eq!(resolved.manifest.name, direct.name);
        assert_eq!(resolved.manifest.rules.len(), direct.rules.len());
    }

    #[test]
    fn override_file_resolves_instead_of_the_embedded_const() {
        let dir = tempfile::tempdir().expect("tempdir");
        let toml = valid_manifest_toml("override-manifest");
        let path = write_tmp(dir.path(), "claude.toml", &toml);

        let source = ManifestSource::new(Some(path.clone()));
        let resolved = source.resolve();

        assert_eq!(resolved.origin, ManifestOrigin::Override(path));
        assert_eq!(resolved.manifest.name, "override-manifest");
        assert_eq!(resolved.digest, sha256_hex(&toml));
    }

    #[test]
    fn editing_the_override_file_changes_the_next_resolve_with_no_rebuild() {
        let dir = tempfile::tempdir().expect("tempdir");
        let first = valid_manifest_toml("first-version");
        let path = write_tmp(dir.path(), "claude.toml", &first);

        let source = ManifestSource::new(Some(path.clone()));
        let resolved_first = source.resolve();
        assert_eq!(resolved_first.manifest.name, "first-version");

        let second = valid_manifest_toml("second-version");
        touch_later(&path, &second);

        let resolved_second = source.resolve();
        assert_eq!(resolved_second.manifest.name, "second-version");
        assert_ne!(resolved_first.digest, resolved_second.digest);
    }

    #[test]
    fn unchanged_mtime_returns_the_cached_compile_without_recompiling() {
        let dir = tempfile::tempdir().expect("tempdir");
        let toml = valid_manifest_toml("cached-version");
        let path = write_tmp(dir.path(), "claude.toml", &toml);

        let source = ManifestSource::new(Some(path));
        let first = source.resolve();
        let second = source.resolve();

        // Same digest both times, and specifically the SAME Arc allocation —
        // proof the second call hit the cache rather than recompiling.
        assert_eq!(first.digest, second.digest);
        assert!(Arc::ptr_eq(&first.manifest, &second.manifest));
    }

    #[test]
    fn malformed_override_keeps_the_last_good_manifest_and_does_not_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let good = valid_manifest_toml("good-version");
        let path = write_tmp(dir.path(), "claude.toml", &good);

        let source = ManifestSource::new(Some(path.clone()));
        let resolved_good = source.resolve();
        assert_eq!(resolved_good.manifest.name, "good-version");

        // Malformed TOML (unterminated string) — compile fails.
        touch_later(&path, "name = \"broken\n[[rules");

        let resolved_after_bad_edit = source.resolve();
        assert_eq!(resolved_after_bad_edit.manifest.name, "good-version");
        assert_eq!(resolved_after_bad_edit.digest, resolved_good.digest);
        assert_eq!(
            resolved_after_bad_edit.origin,
            ManifestOrigin::Override(path)
        );
    }

    #[test]
    fn malformed_override_with_no_prior_good_falls_back_to_embedded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = write_tmp(dir.path(), "claude.toml", "not [ valid toml");

        let source = ManifestSource::new(Some(path));
        let resolved = source.resolve();

        assert_eq!(resolved.origin, ManifestOrigin::Embedded);
        assert_eq!(resolved.digest, sha256_hex(CLAUDE_MANIFEST_TOML));
    }

    #[test]
    fn missing_override_path_falls_back_to_embedded() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("does-not-exist.toml");

        let source = ManifestSource::new(Some(missing));
        let resolved = source.resolve();

        assert_eq!(resolved.origin, ManifestOrigin::Embedded);
    }

    #[test]
    fn from_env_with_unset_var_has_no_override() {
        // SAFETY: test-only env mutation, scoped to this process; no other
        // test in this module reads `MANIFEST_OVERRIDE_PATH_ENV`, and the
        // suite's process-per-test isolation (nextest) means this cannot
        // race a sibling test that does.
        unsafe {
            std::env::remove_var(MANIFEST_OVERRIDE_PATH_ENV);
        }
        let source = ManifestSource::from_env();
        let resolved = source.resolve();
        assert_eq!(resolved.origin, ManifestOrigin::Embedded);
    }

    #[test]
    fn from_env_with_empty_var_has_no_override() {
        // SAFETY: see `from_env_with_unset_var_has_no_override`.
        unsafe {
            std::env::set_var(MANIFEST_OVERRIDE_PATH_ENV, "   ");
        }
        let source = ManifestSource::from_env();
        let resolved = source.resolve();
        assert_eq!(resolved.origin, ManifestOrigin::Embedded);
        unsafe {
            std::env::remove_var(MANIFEST_OVERRIDE_PATH_ENV);
        }
    }

    #[test]
    fn from_env_with_set_var_resolves_the_override_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let toml = valid_manifest_toml("env-configured");
        let path = write_tmp(dir.path(), "claude.toml", &toml);

        // SAFETY: see `from_env_with_unset_var_has_no_override`.
        unsafe {
            std::env::set_var(MANIFEST_OVERRIDE_PATH_ENV, &path);
        }
        let source = ManifestSource::from_env();
        let resolved = source.resolve();
        unsafe {
            std::env::remove_var(MANIFEST_OVERRIDE_PATH_ENV);
        }

        assert_eq!(resolved.manifest.name, "env-configured");
        assert_eq!(resolved.origin, ManifestOrigin::Override(path));
    }
}
