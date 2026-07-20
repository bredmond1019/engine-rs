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

use engine_core::workflows::sdlc_flow::policy::{
    ModelTier, ModelTiers, OutputVerbosity, PartialPolicy, ReviewMode, SdlcPolicy,
};
use engine_core::workflows::sdlc_flow::{policy, profiles};

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
