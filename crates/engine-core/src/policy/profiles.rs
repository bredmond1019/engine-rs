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
use std::path::Path;

use engine_contract::TaskContext;
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::node::NodeError;

/// The `ctx.nodes` identity the resolved policy is stamped under, so every
/// downstream node reads one resolved value rather than re-deriving it.
pub const RESOLVED_POLICY_IDENTITY: &str = "ResolvedPolicy";

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

/// Read the resolved policy stamped by [`stamp_resolved_policy`]. Falls back
/// to `P::default()` when absent or unparsable — the same defensive
/// fallback `sdlc_flow::wrap_up::resolved_policy` uses.
#[must_use]
pub fn resolved_policy<P: DeserializeOwned + Default>(ctx: &TaskContext) -> P {
    ctx.nodes
        .get(RESOLVED_POLICY_IDENTITY)
        .and_then(|value| serde_json::from_value(value.clone()).ok())
        .unwrap_or_default()
}

/// Read `planning/harness.json`'s `<workflow_key>.policy` section (a `P`)
/// out of a worktree, if the file and section exist.
pub fn read_harness_policy_defaults<P: DeserializeOwned>(
    worktree: &Path,
    workflow_key: &str,
) -> Result<Option<P>, NodeError> {
    let harness_path = worktree.join("planning").join("harness.json");
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

/// Read `planning/harness.json`'s `<workflow_key>.profiles` section (a
/// `map<String, P>`) out of a worktree, if the file and section exist.
/// Strips any `_comment*`-keyed sibling entry before deserializing so a
/// documentation comment isn't mistaken for a named profile bundle (see
/// `planning/harness.examples.md`).
pub fn read_harness_profiles<P: DeserializeOwned>(
    worktree: &Path,
    workflow_key: &str,
) -> Result<Option<HashMap<String, P>>, NodeError> {
    let harness_path = worktree.join("planning").join("harness.json");
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

/// Resolve `profile_name` to a `P` bundle, preferring a `harness.json`
/// `<workflow_key>.profiles[name]` entry over `builtin_profile_by_name`.
/// Returns `Ok(None)` when `profile_name` is `None`, and `Err` when a name
/// is given but resolves to neither source (no silent no-op).
pub fn resolve_profile<P: DeserializeOwned>(
    profile_name: Option<&str>,
    worktree: &Path,
    workflow_key: &str,
    builtin_profile_by_name: impl Fn(&str) -> Option<P>,
) -> Result<Option<P>, NodeError> {
    let Some(name) = profile_name else {
        return Ok(None);
    };

    if let Some(harness_profiles) = read_harness_profiles::<P>(worktree, workflow_key)? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-policy-profiles-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
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
    fn resolved_policy_round_trips_via_stamp() {
        let mut ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let policy = TestPartial {
            max_attempts: Some(5),
        };
        stamp_resolved_policy(&mut ctx, &policy).expect("stamp should succeed");
        let read: TestPartial = resolved_policy(&ctx);
        assert_eq!(read, policy);
    }

    #[test]
    fn resolved_policy_falls_back_to_default_when_absent() {
        let ctx = TaskContext {
            event: serde_json::json!({}),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        };
        let read: TestPartial = resolved_policy(&ctx);
        assert_eq!(read, TestPartial::default());
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
}
