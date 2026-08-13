//! Named `PartialApproveAndRunPolicy` profile bundles, the
//! `approve_and_run.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run_from` (`EN.8.D` task 2).
//!
//! Mirrors `operator::queue::policy` (a non-model policy, resolved without
//! any per-run event schema of its own — `APPROVE_AND_RUN` has no setup
//! node in this task; task 5's graph resolves policy directly against a
//! [`PolicyConfigSource`] plus an optional caller-supplied event override).

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{ApproveAndRunPolicy, PartialApproveAndRunPolicy};

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`approve_and_run.policy` / `approve_and_run.profiles`).
const WORKFLOW_KEY: &str = "approve_and_run";

/// The explicit control profile: spelled out explicitly (rather than left
/// all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialApproveAndRunPolicy {
    PartialApproveAndRunPolicy {
        drain_batch_max: Some(60),
        harvest_item_priority: Some(0),
        session_fallback_slug: Some("harvest-review".to_string()),
    }
}

/// Cost/latency floor: a smaller drain batch (fewer records considered per
/// pass, so a pass finishes faster) and a lower-priority default so
/// speed-optimized runs do not crowd out anything already queued at the
/// default priority.
#[must_use]
pub fn cheap_fast() -> PartialApproveAndRunPolicy {
    PartialApproveAndRunPolicy {
        drain_batch_max: Some(20),
        harvest_item_priority: Some(-5),
        session_fallback_slug: Some("harvest-review".to_string()),
    }
}

/// Quality ceiling: the largest drain batch (a single pass covers more of
/// the pending-harvest set) and a higher-priority default so a
/// quality-optimized deployment surfaces harvest decisions ahead of other
/// queued items.
#[must_use]
pub fn thorough() -> PartialApproveAndRunPolicy {
    PartialApproveAndRunPolicy {
        drain_batch_max: Some(100),
        harvest_item_priority: Some(5),
        session_fallback_slug: Some("harvest-review".to_string()),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialApproveAndRunPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `approve_and_run.policy` (a [`PartialApproveAndRunPolicy`]) out of
/// the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialApproveAndRunPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialApproveAndRunPolicy`] bundle,
/// preferring a `harness.json` `approve_and_run.profiles[name]` entry (read
/// via `source`) over the built-in [`profile_by_name`]. Returns `Ok(None)`
/// when `profile_name` is `None`, and `Err` when a name is given but
/// resolves to neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from`, parameterized by [`WORKFLOW_KEY`]
/// and [`profile_by_name`].
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialApproveAndRunPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Resolve the four-layer [`ApproveAndRunPolicy`] for a run: `event_override`
/// (a per-run `policy` field), the resolved `profile_name` bundle, `source`'s
/// `approve_and_run.policy` defaults, and the built-in default, high->low
/// precedence via `crate::policy::resolve`. A [`PolicyConfigSource::Builtin`]
/// source resolves successfully with no filesystem access at all — the case
/// a worktree-free run (channel/API-triggered, or embedded directly in
/// `bastion serve`) needs.
pub fn resolve_policy_for_run_from(
    source: &PolicyConfigSource,
    profile_name: Option<&str>,
    event_override: Option<&PartialApproveAndRunPolicy>,
) -> Result<ApproveAndRunPolicy, NodeError> {
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(profile_name, source)?;
    Ok(crate::policy::resolve(
        ApproveAndRunPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn every_named_profile_sets_every_knob() {
        // A knob absent from the profile bundles is a knob nobody will
        // find — standing rule 6.
        for profile in [baseline(), cheap_fast(), thorough()] {
            assert!(profile.drain_batch_max.is_some());
            assert!(profile.harvest_item_priority.is_some());
            assert!(profile.session_fallback_slug.is_some());
        }
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        assert_eq!(p.drain_batch_max, Some(60));
        assert_eq!(p.harvest_item_priority, Some(0));
        assert_eq!(p.session_fallback_slug, Some("harvest-review".to_string()));
    }

    #[test]
    fn cheap_fast_matches_documented_knob_values() {
        let p = cheap_fast();
        assert_eq!(p.drain_batch_max, Some(20));
        assert_eq!(p.harvest_item_priority, Some(-5));
    }

    #[test]
    fn thorough_matches_documented_knob_values() {
        let p = thorough();
        assert_eq!(p.drain_batch_max, Some(100));
        assert_eq!(p.harvest_item_priority, Some(5));
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_filesystem() {
        let resolved = resolve_policy_for_run_from(&PolicyConfigSource::Builtin, None, None)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, ApproveAndRunPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_applies_named_profile() {
        let resolved =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("cheap-fast"), None)
                .expect("resolve should succeed");
        assert_eq!(resolved.drain_batch_max, 20);
        assert_eq!(resolved.harvest_item_priority, -5);
    }

    #[test]
    fn resolve_policy_for_run_from_unknown_profile_errors() {
        let err =
            resolve_policy_for_run_from(&PolicyConfigSource::Builtin, Some("nonexistent"), None)
                .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_event_override_beats_profile() {
        let event = PartialApproveAndRunPolicy {
            drain_batch_max: Some(7),
            ..Default::default()
        };
        let resolved = resolve_policy_for_run_from(
            &PolicyConfigSource::Builtin,
            Some("thorough"),
            Some(&event),
        )
        .expect("resolve should succeed");
        assert_eq!(resolved.drain_batch_max, 7);
    }

    #[test]
    fn resolve_policy_for_run_from_reads_harness_file_source() {
        let dir = std::env::temp_dir().join(format!(
            "engine-approve-and-run-profiles-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let harness_file = dir.join("harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "approve_and_run": { "policy": { "drain_batch_max": 15 } }
            })
            .to_string(),
        )
        .expect("write harness file");

        let source = PolicyConfigSource::HarnessFile(harness_file);
        let resolved =
            resolve_policy_for_run_from(&source, None, None).expect("resolve should succeed");
        assert_eq!(resolved.drain_batch_max, 15);
        // Untouched knob falls through to builtin.
        assert_eq!(resolved.harvest_item_priority, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn harness_json_approve_and_run_section_parses_into_partial_types() {
        let dir = std::env::temp_dir().join(format!(
            "engine-approve-and-run-profiles-test-parse-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let harness_file = dir.join("harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "approve_and_run": {
                    "policy": {
                        "drain_batch_max": 30
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "drain_batch_max": 45
                        }
                    }
                }
            })
            .to_string(),
        )
        .expect("write harness file");
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let defaults = read_harness_policy_defaults_from(&source)
            .expect("should succeed")
            .expect("policy section present");
        assert_eq!(defaults.drain_batch_max, Some(30));

        let resolved = resolve_profile_from(Some("baseline"), &source)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(resolved.drain_batch_max, Some(45));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_harness_policy_defaults_returns_none_when_section_missing() {
        let dir = std::env::temp_dir().join(format!(
            "engine-approve-and-run-profiles-test-missing-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let harness_file = dir.join("harness.json");
        std::fs::write(&harness_file, serde_json::json!({}).to_string())
            .expect("write harness file");
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let result = read_harness_policy_defaults_from(&source).expect("should succeed");
        assert!(result.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
