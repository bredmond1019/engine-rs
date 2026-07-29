//! Resolved-policy plumbing + workflow-keyed `harness.json` reading and
//! profile-name lookup mechanism, generalized from
//! `workflows::sdlc_flow::setup.rs:30-151` (EN.4.0 task 2).
//!
//! `sdlc_flow` keeps its four concrete named `PartialPolicy` bundles
//! (`profiles::baseline`/`cheap_fast`/`pragmatist`/`batch_reviewer`) in its
//! own `profiles.rs` — only the lookup *mechanism* generalizes here: reading
//! `harness.json`'s `<workflow_key>.policy`/`<workflow_key>.profiles`
//! sections and resolving a named profile against a caller-supplied
//! built-in resolver, parameterized by `workflow_key` so any workflow (not
//! just `"sdlc"`) can share this code.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use engine_contract::TaskContext;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::node::NodeError;

/// The `ctx.nodes` identity the resolved policy is stamped under, so every
/// downstream node reads one resolved value rather than re-deriving it.
pub const RESOLVED_POLICY_IDENTITY: &str = "ResolvedPolicy";

/// Where to look up `harness.json`-sourced policy defaults/profiles for a
/// workflow run, decoupled from a worktree path. Channel- and API-triggered
/// workflows have no repo checkout to read from — [`PolicyConfigSource::Builtin`]
/// lets those resolve successfully (builtin + profile + event layers only)
/// instead of erroring or silently reading `std::env::current_dir()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyConfigSource {
    /// Read `<worktree>/planning/harness.json` — today's on-disk convention.
    Worktree(PathBuf),
    /// Read a specific `harness.json`-shaped file, wherever it lives.
    HarnessFile(PathBuf),
    /// No config file. Resolution falls through to builtin + profile + event
    /// layers only.
    Builtin,
}

impl PolicyConfigSource {
    /// The concrete file path to read, or `None` for [`PolicyConfigSource::Builtin`].
    pub(crate) fn harness_path(&self) -> Option<PathBuf> {
        match self {
            PolicyConfigSource::Worktree(worktree) => {
                Some(worktree.join("planning").join("harness.json"))
            }
            PolicyConfigSource::HarnessFile(path) => Some(path.clone()),
            PolicyConfigSource::Builtin => None,
        }
    }
}

/// Serialize `policy` and stamp it into `ctx.nodes[RESOLVED_POLICY_IDENTITY]`.
pub fn stamp_resolved_policy<P: Serialize>(
    ctx: &mut TaskContext,
    policy: &P,
) -> Result<(), NodeError> {
    let value = serde_json::to_value(policy)
        .map_err(|err| NodeError::new(format!("failed to serialize resolved policy: {err}")))?;
    ctx.nodes
        .insert(RESOLVED_POLICY_IDENTITY.to_string(), value);
    Ok(())
}

/// Read the resolved policy stamped by [`stamp_resolved_policy`], failing
/// loudly instead of silently defaulting: `Err` when the stamp is absent
/// from `ctx.nodes`, or when it is present but fails to deserialize into
/// `P`. The lenient `Default`-falling-back predecessor (`resolved_policy`)
/// was deleted in task 8 — every node caller now goes through this strict
/// read, so a missing/unparsable stamp surfaces as a `NodeError` instead of
/// a silent all-Sonnet resolution.
pub fn resolved_policy_strict<P: DeserializeOwned>(ctx: &TaskContext) -> Result<P, NodeError> {
    let value = ctx.nodes.get(RESOLVED_POLICY_IDENTITY).ok_or_else(|| {
        NodeError::new(format!(
            "no resolved policy stamped under ctx.nodes[{RESOLVED_POLICY_IDENTITY:?}]"
        ))
    })?;
    serde_json::from_value(value.clone()).map_err(|err| {
        NodeError::new(format!(
            "failed to parse resolved policy stamped under {RESOLVED_POLICY_IDENTITY:?}: {err}"
        ))
    })
}

/// Read `<workflow_key>.policy` (a `P`) out of the file addressed by
/// `source`, if the source has a file and the section exists.
/// [`PolicyConfigSource::Builtin`] always yields `Ok(None)` — no filesystem
/// access at all.
pub fn read_harness_policy_defaults_from<P: DeserializeOwned>(
    source: &PolicyConfigSource,
    workflow_key: &str,
) -> Result<Option<P>, NodeError> {
    let Some(harness_path) = source.harness_path() else {
        return Ok(None);
    };
    if !harness_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
        NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
    })?;
    let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
    })?;

    let Some(policy_value) = harness.get(workflow_key).and_then(|v| v.get("policy")) else {
        return Ok(None);
    };

    let partial: P = serde_json::from_value(policy_value.clone()).map_err(|err| {
        NodeError::new(format!(
            "failed to parse {} {workflow_key}.policy: {err}",
            harness_path.display()
        ))
    })?;
    Ok(Some(partial))
}

/// Read `planning/harness.json`'s `<workflow_key>.policy` section (a `P`)
/// out of a worktree, if the file and section exist. Thin wrapper over
/// [`read_harness_policy_defaults_from`] with [`PolicyConfigSource::Worktree`].
pub fn read_harness_policy_defaults<P: DeserializeOwned>(
    worktree: &Path,
    workflow_key: &str,
) -> Result<Option<P>, NodeError> {
    read_harness_policy_defaults_from(
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
        workflow_key,
    )
}

/// Read `<workflow_key>.profiles` (a `map<String, P>`) out of the file
/// addressed by `source`, if the source has a file and the section exists.
/// Strips any `_comment*`-keyed sibling entry before deserializing so a
/// documentation comment isn't mistaken for a named profile bundle (see
/// `planning/harness.examples.md`). [`PolicyConfigSource::Builtin`] always
/// yields `Ok(None)`.
pub fn read_harness_profiles_from<P: DeserializeOwned>(
    source: &PolicyConfigSource,
    workflow_key: &str,
) -> Result<Option<HashMap<String, P>>, NodeError> {
    let Some(harness_path) = source.harness_path() else {
        return Ok(None);
    };
    if !harness_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&harness_path).map_err(|err| {
        NodeError::new(format!("failed to read {}: {err}", harness_path.display()))
    })?;
    let harness: serde_json::Value = serde_json::from_str(&raw).map_err(|err| {
        NodeError::new(format!("failed to parse {}: {err}", harness_path.display()))
    })?;

    let Some(profiles_value) = harness.get(workflow_key).and_then(|v| v.get("profiles")) else {
        return Ok(None);
    };

    let mut profiles_value = profiles_value.clone();
    if let Some(map) = profiles_value.as_object_mut() {
        map.retain(|key, _| !key.starts_with("_comment"));
    }

    let parsed: HashMap<String, P> = serde_json::from_value(profiles_value).map_err(|err| {
        NodeError::new(format!(
            "failed to parse {} {workflow_key}.profiles: {err}",
            harness_path.display()
        ))
    })?;
    Ok(Some(parsed))
}

/// Read `planning/harness.json`'s `<workflow_key>.profiles` section (a
/// `map<String, P>`) out of a worktree, if the file and section exist. Thin
/// wrapper over [`read_harness_profiles_from`] with [`PolicyConfigSource::Worktree`].
pub fn read_harness_profiles<P: DeserializeOwned>(
    worktree: &Path,
    workflow_key: &str,
) -> Result<Option<HashMap<String, P>>, NodeError> {
    read_harness_profiles_from(
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
        workflow_key,
    )
}

/// Resolve `profile_name` to a `P` bundle, preferring a `harness.json`
/// `<workflow_key>.profiles[name]` entry (read via `source`) over
/// `builtin_profile_by_name`. Returns `Ok(None)` when `profile_name` is
/// `None`, and `Err` when a name is given but resolves to neither source (no
/// silent no-op).
pub fn resolve_profile_from<P: DeserializeOwned>(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
    workflow_key: &str,
    builtin_profile_by_name: impl Fn(&str) -> Option<P>,
) -> Result<Option<P>, NodeError> {
    let Some(name) = profile_name else {
        return Ok(None);
    };

    if let Some(harness_profiles) = read_harness_profiles_from::<P>(source, workflow_key)? {
        if let Some(partial) = harness_profiles.into_iter().find(|(key, _)| key == name) {
            return Ok(Some(partial.1));
        }
    }

    if let Some(partial) = builtin_profile_by_name(name) {
        return Ok(Some(partial));
    }

    Err(NodeError::new(format!(
        "unknown profile {name:?}: not found in harness.json {workflow_key}.profiles or built-in profiles"
    )))
}

/// Resolve `profile_name` to a `P` bundle, preferring a `harness.json`
/// `<workflow_key>.profiles[name]` entry over `builtin_profile_by_name`.
/// Returns `Ok(None)` when `profile_name` is `None`, and `Err` when a name
/// is given but resolves to neither source (no silent no-op). Thin wrapper
/// over [`resolve_profile_from`] with [`PolicyConfigSource::Worktree`].
pub fn resolve_profile<P: DeserializeOwned>(
    profile_name: Option<&str>,
    worktree: &Path,
    workflow_key: &str,
    builtin_profile_by_name: impl Fn(&str) -> Option<P>,
) -> Result<Option<P>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
        workflow_key,
        builtin_profile_by_name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    /// Each call yields a directory unique across the whole filesystem, not
    /// merely within this process. `nextest` runs every test in its own OS
    /// process, so a pid-plus-counter scheme (the original approach here)
    /// resets its counter to 0 per test and collides whenever the OS reuses a
    /// pid for a new short-lived test process before the prior test's
    /// (never-cleaned-up) directory of the same name is gone — a stale
    /// `planning/harness.json` from an unrelated test then gets read by this
    /// one. `tempfile::Builder` mints a randomized suffix, so no two calls
    /// (in this process or any other) can land on the same path.
    fn temp_dir() -> std::path::PathBuf {
        tempfile::Builder::new()
            .prefix("engine-core-policy-profiles-test-")
            .tempdir()
            .expect("create temp dir")
            .keep()
    }

    #[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
    struct TestPartial {
        max_attempts: Option<u32>,
    }

    fn builtin(name: &str) -> Option<TestPartial> {
        match name {
            "cheap-fast" => Some(TestPartial {
                max_attempts: Some(1),
            }),
            _ => None,
        }
    }

    #[test]
    fn read_harness_policy_defaults_returns_none_when_file_missing() {
        let worktree = temp_dir();
        let result: Option<TestPartial> =
            read_harness_policy_defaults(&worktree, "sdlc").expect("should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn read_harness_policy_defaults_reads_workflow_keyed_section() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();

        let result: Option<TestPartial> =
            read_harness_policy_defaults(&worktree, "sdlc").expect("should succeed");
        assert_eq!(
            result,
            Some(TestPartial {
                max_attempts: Some(5)
            })
        );
    }

    #[test]
    fn read_harness_profiles_strips_comment_sibling_key() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "sdlc": {
                    "profiles": {
                        "_comment": "explanatory text, not a bundle",
                        "cheap-fast": { "max_attempts": 42 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let profiles: HashMap<String, TestPartial> = read_harness_profiles(&worktree, "sdlc")
            .expect("should succeed")
            .expect("profiles present");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles["cheap-fast"].max_attempts, Some(42));
    }

    #[test]
    fn resolve_profile_none_name_returns_none() {
        let worktree = temp_dir();
        let result: Option<TestPartial> =
            resolve_profile(None, &worktree, "sdlc", builtin).expect("should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_profile_prefers_harness_profile_over_builtin() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "sdlc": { "profiles": { "cheap-fast": { "max_attempts": 99 } } }
            })
            .to_string(),
        )
        .unwrap();

        let result = resolve_profile(Some("cheap-fast"), &worktree, "sdlc", builtin)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result.max_attempts, Some(99));
    }

    #[test]
    fn resolve_profile_falls_back_to_builtin_when_no_harness_entry() {
        let worktree = temp_dir();
        let result = resolve_profile(Some("cheap-fast"), &worktree, "sdlc", builtin)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result.max_attempts, Some(1));
    }

    #[test]
    fn resolve_profile_unknown_name_errors() {
        let worktree = temp_dir();
        let err = resolve_profile(Some("nonexistent"), &worktree, "sdlc", builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn builtin_source_resolves_with_no_filesystem_access() {
        // A `Builtin` source has no `harness_path()` at all, so the reads
        // below can't touch disk regardless of `workflow_key` or cwd.
        let policy: Option<TestPartial> =
            read_harness_policy_defaults_from(&PolicyConfigSource::Builtin, "sdlc")
                .expect("should succeed");
        assert!(policy.is_none());

        let profiles: Option<HashMap<String, TestPartial>> =
            read_harness_profiles_from(&PolicyConfigSource::Builtin, "sdlc")
                .expect("should succeed");
        assert!(profiles.is_none());

        let resolved = resolve_profile_from(
            Some("cheap-fast"),
            &PolicyConfigSource::Builtin,
            "sdlc",
            builtin,
        )
        .expect("should succeed")
        .expect("builtin profile resolved");
        assert_eq!(resolved.max_attempts, Some(1));

        let unknown = resolve_profile_from(
            Some("nonexistent"),
            &PolicyConfigSource::Builtin,
            "sdlc",
            builtin,
        )
        .expect_err("should fail");
        assert!(unknown.message.contains("unknown profile"));
    }

    #[test]
    fn harness_file_source_reads_a_file_outside_any_worktree_layout() {
        let dir = temp_dir();
        // Deliberately not `<dir>/planning/harness.json` — some arbitrary
        // file path with no `planning/` layout at all.
        let harness_file = dir.join("standalone-harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "sdlc": {
                    "policy": { "max_attempts": 7 },
                    "profiles": { "cheap-fast": { "max_attempts": 42 } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let source = PolicyConfigSource::HarnessFile(harness_file);

        let policy: TestPartial = read_harness_policy_defaults_from(&source, "sdlc")
            .expect("should succeed")
            .expect("policy present");
        assert_eq!(policy.max_attempts, Some(7));

        let profiles: HashMap<String, TestPartial> = read_harness_profiles_from(&source, "sdlc")
            .expect("should succeed")
            .expect("profiles present");
        assert_eq!(profiles["cheap-fast"].max_attempts, Some(42));

        let resolved = resolve_profile_from(Some("cheap-fast"), &source, "sdlc", builtin)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(resolved.max_attempts, Some(42));
    }

    #[test]
    fn worktree_source_behaves_identically_to_today() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({ "sdlc": { "policy": { "max_attempts": 5 } } }).to_string(),
        )
        .unwrap();

        let source = PolicyConfigSource::Worktree(worktree.clone());
        let via_from: Option<TestPartial> =
            read_harness_policy_defaults_from(&source, "sdlc").expect("should succeed");
        let via_wrapper: Option<TestPartial> =
            read_harness_policy_defaults(&worktree, "sdlc").expect("should succeed");
        assert_eq!(via_from, via_wrapper);
        assert_eq!(
            via_from,
            Some(TestPartial {
                max_attempts: Some(5)
            })
        );
    }

    #[test]
    fn resolved_policy_strict_errors_on_absent_stamp() {
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let err = resolved_policy_strict::<TestPartial>(&ctx).expect_err("should fail");
        assert!(err.message.contains("no resolved policy stamped"));
    }

    #[test]
    fn resolved_policy_strict_errors_on_unparsable_stamp() {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        // `max_attempts` expects a number; stamp a string so deserialization fails.
        ctx.nodes.insert(
            RESOLVED_POLICY_IDENTITY.to_string(),
            serde_json::json!({ "max_attempts": "not-a-number" }),
        );
        let err = resolved_policy_strict::<TestPartial>(&ctx).expect_err("should fail");
        assert!(err.message.contains("failed to parse resolved policy"));
    }

    #[test]
    fn resolved_policy_strict_succeeds_on_valid_stamp() {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let policy = TestPartial {
            max_attempts: Some(9),
        };
        stamp_resolved_policy(&mut ctx, &policy).expect("stamp should succeed");
        let read: TestPartial = resolved_policy_strict(&ctx).expect("should succeed");
        assert_eq!(read, policy);
    }

    #[test]
    fn read_harness_profiles_from_strips_comment_sibling_key() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "sdlc": {
                    "profiles": {
                        "_comment": "explanatory text, not a bundle",
                        "cheap-fast": { "max_attempts": 42 }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let profiles: HashMap<String, TestPartial> =
            read_harness_profiles_from(&PolicyConfigSource::Worktree(worktree), "sdlc")
                .expect("should succeed")
                .expect("profiles present");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles["cheap-fast"].max_attempts, Some(42));
    }
}
