//! Deterministic plumbing tests for the four canonical SDLC Flow policy
//! profiles (`Phase 2 · Block C`, task 1): proves each named profile
//! resolves to the documented `SdlcPolicy`, that `profile_by_name` round
//! trips all four kebab-case names (plus `None` for unknown names) to the
//! same resolved policy the direct constructors produce.
//!
//! Hermetic by construction: no `resolve` call here touches disk, a
//! subprocess, or the network — it is pure in-memory struct merging. Later
//! tasks in this block extend this file with precedence, unknown-profile
//! error, and routing assertions (see `planning/plan-sdlc-policy-profiles-C/`).

use std::collections::HashMap;

use engine_contract::TaskContext;
use engine_core::workflows::sdlc_flow::policy::{
    ModelTier, ModelTiers, OutputVerbosity, PartialPolicy, ReviewMode, SdlcPolicy,
};
use engine_core::workflows::sdlc_flow::{policy, profiles, setup};

/// A profile's kebab-case name paired with its direct constructor, used to
/// prove `profile_by_name` round-trips to the same resolution.
type NamedProfileCtor = (&'static str, fn() -> PartialPolicy);

/// `baseline` is the explicit control: Sonnet on every tier, `per_task`
/// review, `llm_triage` off — a legible no-op against the built-in default.
#[test]
fn baseline_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::baseline()),
        None,
    );

    assert_eq!(
        resolved.model_tiers,
        ModelTiers {
            implement: ModelTier::Sonnet,
            implement_simple: ModelTier::Sonnet,
            review: ModelTier::Sonnet,
            triage: ModelTier::Sonnet,
            generate: ModelTier::Sonnet,
        }
    );
    assert_eq!(resolved.review_mode, ReviewMode::PerTask);
    assert!(!resolved.llm_triage);
}

/// `cheap-fast`: Haiku implement, local triage+review, terse output,
/// trivial-task review skip.
#[test]
fn cheap_fast_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::cheap_fast()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
    assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
}

/// `pragmatist`: Sonnet implement, local review, prompt caching on,
/// trivial-task review skip, `llm_triage` on.
#[test]
fn pragmatist_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::pragmatist()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Sonnet);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert!(resolved.prompt_cache);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    assert!(resolved.llm_triage);
}

/// `batch-reviewer`: Sonnet implement, per-task review collapsed into a
/// single end-of-run review.
#[test]
fn batch_reviewer_profile_resolves_to_documented_policy() {
    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::batch_reviewer()),
        None,
    );

    assert_eq!(resolved.model_tiers.implement, ModelTier::Sonnet);
    assert_eq!(resolved.review_mode, ReviewMode::EndOnly);
}

/// `profile_by_name` must resolve all four canonical kebab-case names to a
/// bundle that produces the exact same resolved `SdlcPolicy` as calling the
/// named constructor directly.
#[test]
fn profile_by_name_matches_direct_constructor_resolution() {
    let cases: &[NamedProfileCtor] = &[
        ("baseline", profiles::baseline),
        ("cheap-fast", profiles::cheap_fast),
        ("pragmatist", profiles::pragmatist),
        ("batch-reviewer", profiles::batch_reviewer),
    ];

    for (name, ctor) in cases {
        let by_name = profiles::profile_by_name(name)
            .unwrap_or_else(|| panic!("profile_by_name({name:?}) should resolve to Some(bundle)"));
        let direct = ctor();

        let resolved_by_name = policy::resolve(SdlcPolicy::default(), None, Some(&by_name), None);
        let resolved_direct = policy::resolve(SdlcPolicy::default(), None, Some(&direct), None);

        assert_eq!(
            resolved_by_name, resolved_direct,
            "profile_by_name({name:?}) should resolve identically to the direct constructor"
        );
    }
}

/// An unknown profile name must not silently resolve to a bundle.
#[test]
fn profile_by_name_returns_none_for_unknown_name() {
    assert_eq!(profiles::profile_by_name("does-not-exist"), None);
}

/// A bare `TaskContext` carrying only an `event` — mirrors `setup.rs`'s
/// private `empty_context` test helper, reconstructed here since it isn't
/// exported across the crate boundary.
fn empty_context(event: serde_json::Value) -> TaskContext {
    TaskContext {
        event,
        nodes: HashMap::new(),
        metadata: serde_json::json!({}),
        node_runs: HashMap::new(),
    }
}

/// Event-inline `policy` overrides beat the `profile` layer field-by-field:
/// the overridden field (`max_attempts`) takes the inline value, while every
/// other field still falls through to the profile's bundle. Exercised
/// through the same `resolve` call shape `setup::resolve_policy_for_run`
/// uses (profile arg = `Some(cheap_fast)`, event_override = `Some(inline)`).
#[test]
fn event_inline_policy_overrides_profile_field_but_keeps_profile_tiers() {
    let inline_override = PartialPolicy {
        max_attempts: Some(9),
        ..Default::default()
    };

    let resolved = policy::resolve(
        SdlcPolicy::default(),
        None,
        Some(&profiles::cheap_fast()),
        Some(&inline_override),
    );

    // The inline-overridden field wins...
    assert_eq!(resolved.max_attempts, 9);
    // ...but every other cheap-fast knob still comes through from the
    // profile layer, since the inline override left those fields `None`.
    assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
    assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
    assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
    assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
}

/// The full run-level resolution path (`setup::resolve_policy_for_run`,
/// reachable here since `setup` is a `pub mod` and the function itself is
/// `pub`) errors on an unknown profile name rather than silently no-op'ing.
/// `setup.rs`'s own unit test (`unknown_profile_name_returns_node_error`)
/// covers the same path from inside the crate; this integration-level copy
/// proves the behavior holds through the public API surface too.
#[test]
fn resolve_policy_for_run_errors_on_unknown_profile_name() {
    let worktree = std::env::temp_dir().join(format!(
        "sdlc_flow_profiles_unknown_profile_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&worktree).expect("temp worktree dir should create");

    let ctx = empty_context(serde_json::json!({
        "spec_slug": "my-spec",
        "profile": "does-not-exist",
    }));

    let err = setup::resolve_policy_for_run(&ctx, &worktree)
        .expect_err("an unknown profile name must not resolve");
    assert!(
        err.message.contains("unknown profile"),
        "expected an 'unknown profile' error message, got: {}",
        err.message
    );

    let _ = std::fs::remove_dir_all(&worktree);
}
