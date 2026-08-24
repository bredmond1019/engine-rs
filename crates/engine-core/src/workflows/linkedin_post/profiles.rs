//! Named `LinkedInPostPolicy` profile bundles, the `linkedin_post.{policy,
//! profiles}` `harness.json` section reader, and `resolve_policy_for_run`.
//!
//! Mirrors `content_pipeline::profiles` (the named-bundle catalog +
//! `read_harness_policy_defaults`/`resolve_profile`/`resolve_policy_for_run`
//! trio), generalized over the shared `crate::policy::profiles` plumbing
//! (EN.4.0).
//!
//! Three named profiles, per CLAUDE.md standing rule 6 — every profile sets
//! every knob explicitly:
//! - `baseline` — explicit no-op against the built-in default.
//! - `cheap-fast` — the cost/latency floor: Haiku everywhere, iteration cap
//!   1, translate off.
//! - `thorough` — the quality ceiling: Opus draft/critic, Sonnet translate,
//!   a higher iteration cap, translate on.
//!
//! Caller-supplied loop bounds (`max_critic_iterations`, `candidate_count`)
//! are validated on the *final* resolved policy — after all four layers
//! have merged — via `policy::validate_bounds`, so an out-of-range value
//! from any layer (harness defaults, a named profile, or a per-event
//! override) surfaces as a rejected run rather than a silently accepted or
//! clamped one.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{
    self, validate_bounds, LinkedInPostPolicy, ModelTier, PartialLinkedInPostPolicy,
    PartialModelTiers,
};
use super::schema::LinkedInPostEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`linkedin_post.policy` / `linkedin_post.profiles`), passed to
/// the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "linkedin_post";

/// The explicit control profile: all-Sonnet, default loop bounds (3
/// critic iterations, 3 candidates), translate on. Spelled out explicitly
/// (rather than left all-`None`) so selecting `profile: "baseline"` is a
/// legible, self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialLinkedInPostPolicy {
    PartialLinkedInPostPolicy {
        model_tiers: Some(PartialModelTiers {
            draft: Some(ModelTier::Sonnet),
            critic: Some(ModelTier::Sonnet),
            translate: Some(ModelTier::Sonnet),
        }),
        local: None,
        max_critic_iterations: Some(3),
        candidate_count: Some(3),
        translate_enabled: Some(true),
    }
}

/// The cost/latency floor: Haiku on every stage, a single critic pass
/// (`max_critic_iterations = 1`), and the PT translate pass turned off —
/// `TranslateNode` still stays in the declared graph (standing rule 6),
/// it just takes its no-op path.
#[must_use]
pub fn cheap_fast() -> PartialLinkedInPostPolicy {
    PartialLinkedInPostPolicy {
        model_tiers: Some(PartialModelTiers {
            draft: Some(ModelTier::Haiku),
            critic: Some(ModelTier::Haiku),
            translate: Some(ModelTier::Haiku),
        }),
        local: None,
        max_critic_iterations: Some(1),
        candidate_count: Some(3),
        translate_enabled: Some(false),
    }
}

/// The quality ceiling: Opus for drafting and critiquing, a generous
/// revise-loop cap, and the PT translate pass on.
#[must_use]
pub fn thorough() -> PartialLinkedInPostPolicy {
    PartialLinkedInPostPolicy {
        model_tiers: Some(PartialModelTiers {
            draft: Some(ModelTier::Opus),
            critic: Some(ModelTier::Opus),
            translate: Some(ModelTier::Sonnet),
        }),
        local: None,
        max_critic_iterations: Some(5),
        candidate_count: Some(3),
        translate_enabled: Some(true),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns
/// `None` for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialLinkedInPostPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Deserialize the inbound `LINKEDIN_POST` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<LinkedInPostEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid LINKEDIN_POST event: {err}")))
}

/// Read `linkedin_post.policy` (a [`PartialLinkedInPostPolicy`]) out of the
/// file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D),
/// parameterized by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialLinkedInPostPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialLinkedInPostPolicy`] bundle, preferring a `harness.json`
/// `linkedin_post.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries
/// no `profile` field, and `Err` when a `profile` name is given but
/// resolves to neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialLinkedInPostPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `linkedin_post.policy` section out of a
/// worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already
/// had a worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialLinkedInPostPolicy>, NodeError> {
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
) -> Result<Option<PartialLinkedInPostPolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`LinkedInPostPolicy`] for this run against an
/// arbitrary [`PolicyConfigSource`]: the inbound event's `policy`
/// override, the resolved `profile` bundle, `source`'s `linkedin_post.
/// policy` defaults, and the built-in default, high->low precedence via
/// [`policy::resolve`]. A [`PolicyConfigSource::Builtin`] source resolves
/// successfully with no filesystem access.
///
/// The resolved policy's loop bounds are then validated via
/// [`validate_bounds`]: an out-of-range `max_critic_iterations` or
/// `candidate_count` (from any layer) surfaces as `Err` rather than being
/// silently clamped or accepted.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<LinkedInPostPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    let resolved = policy::resolve(
        LinkedInPostPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    );
    validate_bounds(&resolved).map_err(NodeError::new)?;
    Ok(resolved)
}

/// Resolve the four-layer [`LinkedInPostPolicy`] for this run: the inbound
/// event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `linkedin_post.policy` defaults, and
/// the built-in default, high->low precedence via [`policy::resolve`].
/// Thin wrapper over [`resolve_policy_for_run_from`] with
/// [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<LinkedInPostPolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use crate::policy::{resolved_policy_strict, stamp_resolved_policy};

    use super::*;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-linkedin-post-profiles-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn minimal_event() -> serde_json::Value {
        serde_json::json!({
            "since": "2026-08-17",
            "until": "2026-08-24",
        })
    }

    fn base_ctx(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
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
    fn every_named_profile_sets_every_knob_explicitly() {
        for (name, bundle) in [
            ("baseline", baseline()),
            ("cheap-fast", cheap_fast()),
            ("thorough", thorough()),
        ] {
            let tiers = bundle
                .model_tiers
                .unwrap_or_else(|| panic!("profile '{name}' must state model_tiers"));
            assert!(
                tiers.draft.is_some(),
                "profile '{name}' must state draft tier"
            );
            assert!(
                tiers.critic.is_some(),
                "profile '{name}' must state critic tier"
            );
            assert!(
                tiers.translate.is_some(),
                "profile '{name}' must state translate tier"
            );
            assert!(
                bundle.max_critic_iterations.is_some(),
                "profile '{name}' must state max_critic_iterations"
            );
            assert!(
                bundle.candidate_count.is_some(),
                "profile '{name}' must state candidate_count"
            );
            assert!(
                bundle.translate_enabled.is_some(),
                "profile '{name}' must state translate_enabled"
            );
        }
    }

    #[test]
    fn baseline_resolves_to_the_builtin_default() {
        let resolved =
            policy::resolve(LinkedInPostPolicy::default(), None, Some(&baseline()), None);
        assert_eq!(
            resolved,
            LinkedInPostPolicy::default(),
            "baseline must be an explicit no-op against the built-in default"
        );
    }

    #[test]
    fn cheap_fast_is_the_cost_latency_floor() {
        let resolved = policy::resolve(
            LinkedInPostPolicy::default(),
            None,
            Some(&cheap_fast()),
            None,
        );
        assert_eq!(resolved.model_tiers.draft, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Haiku);
        assert_eq!(resolved.max_critic_iterations, 1);
        assert!(!resolved.translate_enabled);
    }

    #[test]
    fn thorough_is_the_quality_ceiling() {
        let resolved =
            policy::resolve(LinkedInPostPolicy::default(), None, Some(&thorough()), None);
        assert_eq!(resolved.model_tiers.draft, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Opus);
        assert!(
            resolved.max_critic_iterations > LinkedInPostPolicy::default().max_critic_iterations
        );
        assert!(resolved.translate_enabled);
    }

    #[test]
    fn harness_json_linkedin_post_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "linkedin_post": {
                    "policy": {
                        "model_tiers": { "draft": "haiku" }
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "model_tiers": {
                                "draft": "sonnet",
                                "critic": "sonnet",
                                "translate": "sonnet"
                            }
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let defaults = read_harness_policy_defaults(&worktree)
            .expect("should succeed")
            .expect("policy section present");
        assert_eq!(defaults.model_tiers.unwrap().draft, Some(ModelTier::Haiku));

        let resolved = resolve_profile(Some("baseline"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(resolved.model_tiers.unwrap().draft, Some(ModelTier::Sonnet));
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
        let result = resolve_profile(Some("cheap-fast"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result, cheap_fast());
    }

    #[test]
    fn resolve_profile_unknown_name_errors() {
        let worktree = temp_dir();
        let err = resolve_profile(Some("nonexistent"), &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_with_no_overrides_returns_builtin_default() {
        let worktree = temp_dir();
        let ctx = base_ctx(minimal_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, LinkedInPostPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_cheap_fast_profile() {
        let worktree = temp_dir();
        let mut event = minimal_event();
        event["profile"] = serde_json::json!("cheap-fast");
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.draft, ModelTier::Haiku);
        assert!(!resolved.translate_enabled);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "linkedin_post": {
                    "policy": { "model_tiers": { "critic": "haiku" } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = minimal_event();
        event["policy"] = serde_json::json!({
            "model_tiers": { "critic": "opus" }
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.critic, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_unknown_profile_name_errors() {
        let worktree = temp_dir();
        let mut event = minimal_event();
        event["profile"] = serde_json::json!("nonexistent");
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_rejects_out_of_range_max_critic_iterations_override() {
        let worktree = temp_dir();
        let mut event = minimal_event();
        event["policy"] = serde_json::json!({ "max_critic_iterations": 999 });
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree)
            .expect_err("out-of-range max_critic_iterations should be rejected");
        assert!(err.message.contains("max_critic_iterations"));
    }

    #[test]
    fn resolve_policy_for_run_rejects_out_of_range_candidate_count_override() {
        let worktree = temp_dir();
        let mut event = minimal_event();
        event["policy"] = serde_json::json!({ "candidate_count": 999 });
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree)
            .expect_err("out-of-range candidate_count should be rejected");
        assert!(err.message.contains("candidate_count"));
    }

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = base_ctx(minimal_event());
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, LinkedInPostPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_still_errors_on_unknown_profile() {
        let mut event = minimal_event();
        event["profile"] = serde_json::json!("nonexistent");
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_harness_file_source_preserves_precedence() {
        let dir = temp_dir();
        let harness_file = dir.join("standalone-harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "linkedin_post": { "policy": { "model_tiers": { "critic": "haiku" } } }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let mut event = minimal_event();
        event["policy"] = serde_json::json!({
            "model_tiers": { "critic": "opus" }
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.model_tiers.critic, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = minimal_event();
        event["profile"] = serde_json::json!("cheap-fast");
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }

    // --- ctx.nodes stamping (RunTelemetry/PolicyAggregate attribution) -----

    #[test]
    fn resolved_policy_stamps_into_ctx_nodes_and_reads_back() {
        let worktree = temp_dir();
        let ctx = base_ctx(minimal_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");

        let mut ctx = ctx;
        stamp_resolved_policy(&mut ctx, &resolved).expect("stamp should succeed");

        let read_back: LinkedInPostPolicy =
            resolved_policy_strict(&ctx).expect("resolved policy should be readable back");
        assert_eq!(read_back, resolved);
    }
}
