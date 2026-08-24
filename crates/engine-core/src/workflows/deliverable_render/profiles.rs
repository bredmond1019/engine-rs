//! Named `DeliverableRenderPolicy` profile bundles, the
//! `deliverable_render.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run`.
//!
//! Mirrors `proposal_generator::profiles` (the named-bundle catalog +
//! `read_harness_policy_defaults`/`resolve_profile`/`resolve_policy_for_run`
//! trio), generalized over the shared `crate::policy::profiles` plumbing
//! (EN.4.0). There is no setup node in this workflow — `RenderDeliverableNode`
//! calls [`resolve_policy_for_run`] directly.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{self, DeliverableRenderPolicy, ModelTier, PartialDeliverableRenderPolicy};
use super::schema::DeliverableRenderEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`deliverable_render.policy` / `deliverable_render.profiles`),
/// passed to the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "deliverable_render";

/// The explicit control profile: the polish pass off. Spelled out
/// explicitly (rather than left all-`None`) so selecting `profile:
/// "baseline"` is a legible, self-documenting no-op against the built-in
/// default.
#[must_use]
pub fn baseline() -> PartialDeliverableRenderPolicy {
    PartialDeliverableRenderPolicy {
        polish_enabled: Some(false),
        polish_model_tier: Some(ModelTier::Sonnet),
    }
}

/// Cheapest/fastest profile: polish pass stays off — the render is already
/// the cost/latency floor with no model call on this path.
#[must_use]
pub fn cheap_fast() -> PartialDeliverableRenderPolicy {
    PartialDeliverableRenderPolicy {
        polish_enabled: Some(false),
        polish_model_tier: Some(ModelTier::Sonnet),
    }
}

/// Quality-ceiling profile: the optional model-polish pass runs over the
/// rendered markdown before it is written to disk.
#[must_use]
pub fn thorough() -> PartialDeliverableRenderPolicy {
    PartialDeliverableRenderPolicy {
        polish_enabled: Some(true),
        polish_model_tier: Some(ModelTier::Sonnet),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialDeliverableRenderPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Deserialize the inbound `DELIVERABLE_RENDER` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<DeliverableRenderEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid DELIVERABLE_RENDER event: {err}")))
}

/// Read `deliverable_render.policy` (a [`PartialDeliverableRenderPolicy`])
/// out of the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D), parameterized
/// by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialDeliverableRenderPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialDeliverableRenderPolicy`] bundle, preferring a `harness.json`
/// `deliverable_render.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries no
/// `profile` field, and `Err` when a `profile` name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialDeliverableRenderPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `deliverable_render.policy` section out of
/// a worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already had a
/// worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialDeliverableRenderPolicy>, NodeError> {
    read_harness_policy_defaults_from(&PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

/// Resolve `event.profile` against a worktree. Thin wrapper over
/// [`resolve_profile_from`] with [`PolicyConfigSource::Worktree`], kept so
/// tests/callers that already had a worktree path in hand don't need to
/// construct a source.
#[cfg(test)]
fn resolve_profile(
    profile_name: Option<&str>,
    worktree: &Path,
) -> Result<Option<PartialDeliverableRenderPolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`DeliverableRenderPolicy`] for this run against
/// an arbitrary [`PolicyConfigSource`]: the inbound event's `policy`
/// override, the resolved `profile` bundle, `source`'s
/// `deliverable_render.policy` defaults, and the built-in default, high->low
/// precedence via [`policy::resolve`]. A [`PolicyConfigSource::Builtin`]
/// source resolves successfully with no filesystem access — the case a
/// worktree-free (channel/API-triggered) run needs.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<DeliverableRenderPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    Ok(policy::resolve(
        DeliverableRenderPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

/// Resolve the four-layer [`DeliverableRenderPolicy`] for this run: the
/// inbound event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `deliverable_render.policy` defaults,
/// and the built-in default, high->low precedence via [`policy::resolve`].
/// This is what `RenderDeliverableNode` calls — there is no dedicated setup
/// node in this workflow. Thin wrapper over [`resolve_policy_for_run_from`]
/// with [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<DeliverableRenderPolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::locale::Locale;
    use crate::policy::{resolved_policy_strict, stamp_resolved_policy};
    use crate::workflows::proposal_generator::schema::AutomationRoadmap;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-deliverable-render-profiles-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn base_event() -> DeliverableRenderEventSchema {
        DeliverableRenderEventSchema {
            roadmap: AutomationRoadmap::default(),
            locale: Locale::default(),
            output_dir: std::path::PathBuf::from("/tmp/out"),
            policy: None,
            profile: None,
        }
    }

    fn base_ctx(event: DeliverableRenderEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

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
    fn every_profile_sets_the_polish_knob_explicitly() {
        for bundle in [baseline(), cheap_fast(), thorough()] {
            assert!(
                bundle.polish_enabled.is_some(),
                "every named profile must set polish_enabled explicitly"
            );
        }
    }

    #[test]
    fn baseline_and_cheap_fast_keep_polish_off() {
        assert_eq!(baseline().polish_enabled, Some(false));
        assert_eq!(cheap_fast().polish_enabled, Some(false));
    }

    #[test]
    fn thorough_turns_polish_on() {
        assert_eq!(thorough().polish_enabled, Some(true));
    }

    #[test]
    fn resolve_policy_for_run_with_no_overrides_returns_builtin_default() {
        let worktree = temp_dir();
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, DeliverableRenderPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_thorough_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("thorough".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert!(resolved.polish_enabled);
    }

    #[test]
    fn resolve_policy_for_run_applies_named_baseline_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("baseline".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert!(!resolved.polish_enabled);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "deliverable_render": {
                    "policy": { "polish_enabled": true }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = base_event();
        event.policy = Some(PartialDeliverableRenderPolicy {
            polish_enabled: Some(false),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert!(!resolved.polish_enabled);
    }

    #[test]
    fn resolve_policy_for_run_unknown_profile_name_errors() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("nonexistent".to_string());
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn read_harness_policy_defaults_returns_none_when_file_missing() {
        let worktree = temp_dir();
        let result = read_harness_policy_defaults(&worktree).expect("should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_profile_falls_back_to_builtin_when_no_harness_entry() {
        let worktree = temp_dir();
        let result = resolve_profile(Some("thorough"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result, thorough());
    }

    #[test]
    fn harness_json_deliverable_render_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "deliverable_render": {
                    "policy": {
                        "polish_enabled": false,
                        "polish_model_tier": "haiku"
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "thorough": { "polish_enabled": true, "polish_model_tier": "opus" }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let defaults = read_harness_policy_defaults(&worktree)
            .expect("should succeed")
            .expect("policy section present");
        assert_eq!(defaults.polish_enabled, Some(false));
        assert_eq!(defaults.polish_model_tier, Some(ModelTier::Haiku));

        let resolved = resolve_profile(Some("thorough"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(resolved.polish_model_tier, Some(ModelTier::Opus));
    }

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, DeliverableRenderPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_still_errors_on_unknown_profile() {
        let mut event = base_event();
        event.profile = Some("nonexistent".to_string());
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("thorough".to_string());
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }

    // --- ctx.nodes stamping (CLAUDE.md standing rule 6: "Stamp the resolved
    // value into the node's ctx.nodes result so RunTelemetry/PolicyAggregate
    // can attribute cost") -------------------------------------------------

    #[test]
    fn resolved_policy_round_trips_through_ctx_nodes_stamp() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("thorough".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");

        let mut stamped_ctx = ctx;
        stamp_resolved_policy(&mut stamped_ctx, &resolved).expect("stamp should succeed");

        let read_back: DeliverableRenderPolicy =
            resolved_policy_strict(&stamped_ctx).expect("stamp should be readable");
        assert_eq!(read_back, resolved);
        assert!(read_back.polish_enabled);
    }
}
