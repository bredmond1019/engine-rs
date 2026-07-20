//! Named policy profiles — four canonical `PartialPolicy` bundles tuned for
//! distinct cost/time/quality tradeoffs, resolved by name via the `profile:`
//! event field (see `planning/sdlc-flow-policy-research/notes.md` ->
//! *Proposed Test Profiles* for the source knob values).
//!
//! These sit in the resolution chain between `harness.json`'s `sdlc.policy`
//! defaults and an event's inline `policy` override (see `policy::resolve`).
//! Every field left `None` here falls through to whatever layer is applied
//! below it (harness defaults, then the built-in default).

use super::policy::{ModelTier, OutputVerbosity, PartialModelTiers, PartialPolicy, ReviewMode};

/// The explicit control profile: Sonnet on every tier, `per_task` review,
/// `llm_triage` off. Spelled out explicitly (rather than left all-`None`)
/// so selecting `profile: "baseline"` is a legible, self-documenting no-op
/// against the built-in default.
#[must_use]
pub fn baseline() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            implement_simple: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Sonnet),
            triage: Some(ModelTier::Sonnet),
            generate: Some(ModelTier::Sonnet),
        }),
        review_mode: Some(ReviewMode::PerTask),
        llm_triage: Some(false),
        ..Default::default()
    }
}

/// Cheapest/fastest profile: `haiku` implement, local triage+review, terse
/// output, trivial-task review skip.
#[must_use]
pub fn cheap_fast() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Haiku),
            triage: Some(ModelTier::Local),
            review: Some(ModelTier::Local),
            ..Default::default()
        }),
        output_verbosity: Some(OutputVerbosity::Terse),
        review_mode: Some(ReviewMode::TrivialSkip),
        ..Default::default()
    }
}

/// Balanced profile: `sonnet` implement, local review, prompt caching on,
/// trivial-task review skip, `llm_triage` on.
#[must_use]
pub fn pragmatist() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Local),
            ..Default::default()
        }),
        prompt_cache: Some(true),
        review_mode: Some(ReviewMode::TrivialSkip),
        llm_triage: Some(true),
        ..Default::default()
    }
}

/// Batch profile: `sonnet` implement, per-task review collapsed into a
/// single end-of-run review.
#[must_use]
pub fn batch_reviewer() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            ..Default::default()
        }),
        review_mode: Some(ReviewMode::EndOnly),
        ..Default::default()
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the four canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "pragmatist" => Some(pragmatist()),
        "batch-reviewer" => Some(batch_reviewer()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_by_name_resolves_all_four_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("pragmatist"), Some(pragmatist()));
        assert_eq!(profile_by_name("batch-reviewer"), Some(batch_reviewer()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn cheap_fast_matches_documented_knob_values() {
        let p = cheap_fast();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Haiku));
        assert_eq!(tiers.triage, Some(ModelTier::Local));
        assert_eq!(tiers.review, Some(ModelTier::Local));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(p.review_mode, Some(ReviewMode::TrivialSkip));
    }

    #[test]
    fn pragmatist_matches_documented_knob_values() {
        let p = pragmatist();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(tiers.review, Some(ModelTier::Local));
        assert_eq!(p.prompt_cache, Some(true));
        assert_eq!(p.review_mode, Some(ReviewMode::TrivialSkip));
        assert_eq!(p.llm_triage, Some(true));
    }

    #[test]
    fn batch_reviewer_matches_documented_knob_values() {
        let p = batch_reviewer();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(p.review_mode, Some(ReviewMode::EndOnly));
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(tiers.review, Some(ModelTier::Sonnet));
        assert_eq!(tiers.triage, Some(ModelTier::Sonnet));
        assert_eq!(tiers.generate, Some(ModelTier::Sonnet));
        assert_eq!(p.review_mode, Some(ReviewMode::PerTask));
        assert_eq!(p.llm_triage, Some(false));
    }
}
