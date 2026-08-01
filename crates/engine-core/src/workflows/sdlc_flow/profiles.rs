//! Named policy profiles — four canonical `PartialPolicy` bundles tuned for
//! distinct cost/time/quality tradeoffs, resolved by name via the `profile:`
//! event field (see `planning/sdlc-flow-policy-research/notes.md` ->
//! *Proposed Test Profiles* for the source knob values).
//!
//! These sit in the resolution chain between `harness.json`'s `sdlc.policy`
//! defaults and an event's inline `policy` override (see `policy::resolve`).
//! Every field left `None` here falls through to whatever layer is applied
//! below it (harness defaults, then the built-in default).

use super::policy::{
    ModelTier, OutputVerbosity, PartialModelTiers, PartialPolicy, ReviewMode, TestDepth,
};

/// The explicit control profile: Sonnet on every tier, `per_task` review,
/// `llm_triage` off, `test_depth: full`. Spelled out explicitly (rather than
/// left all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            implement_simple: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Sonnet),
            triage: Some(ModelTier::Sonnet),
            // `generate` is Opus in the built-in default (it is what
            // `GenerateTasksNode` actually runs); baseline must restate that
            // exactly, or selecting it would silently downgrade the stage.
            generate: Some(ModelTier::Opus),
            docs: Some(ModelTier::Sonnet),
        }),
        review_mode: Some(ReviewMode::PerTask),
        llm_triage: Some(false),
        test_depth: Some(TestDepth::Full),
        ..Default::default()
    }
}

/// Cheapest/fastest profile: `haiku` implement, local triage+review, terse
/// output, trivial-task review skip, `test_depth: fast` (the cost/latency
/// floor — per-task check selection is the single largest lever in this
/// repo, per CLAUDE.md's measured 2m44s -> 6.4s).
#[must_use]
pub fn cheap_fast() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Haiku),
            triage: Some(ModelTier::Local),
            review: Some(ModelTier::Local),
            // Both agentic/task-authoring stages drop to Haiku — the cost
            // floor. Deliberately NOT `Local`: the local tier is scoped to
            // single-shot judgment calls, not an agentic docs writer or the
            // task-generation call (doctrine at `graph.rs`'s
            // `registry_for_policy`).
            generate: Some(ModelTier::Haiku),
            docs: Some(ModelTier::Haiku),
            ..Default::default()
        }),
        output_verbosity: Some(OutputVerbosity::Terse),
        review_mode: Some(ReviewMode::TrivialSkip),
        test_depth: Some(TestDepth::Fast),
        ..Default::default()
    }
}

/// Balanced profile: `sonnet` implement, local review, prompt caching on,
/// trivial-task review skip, `llm_triage` on, `test_depth: fast`.
#[must_use]
pub fn pragmatist() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Local),
            generate: Some(ModelTier::Opus),
            docs: Some(ModelTier::Sonnet),
            ..Default::default()
        }),
        prompt_cache: Some(true),
        review_mode: Some(ReviewMode::TrivialSkip),
        llm_triage: Some(true),
        test_depth: Some(TestDepth::Fast),
        ..Default::default()
    }
}

/// Batch profile: `sonnet` implement, per-task review collapsed into a
/// single end-of-run review, `test_depth: fast`.
#[must_use]
pub fn batch_reviewer() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            generate: Some(ModelTier::Opus),
            docs: Some(ModelTier::Sonnet),
            ..Default::default()
        }),
        review_mode: Some(ReviewMode::EndOnly),
        test_depth: Some(TestDepth::Fast),
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
        assert_eq!(tiers.generate, Some(ModelTier::Haiku));
        assert_eq!(tiers.docs, Some(ModelTier::Haiku));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(p.review_mode, Some(ReviewMode::TrivialSkip));
        assert_eq!(p.test_depth, Some(TestDepth::Fast));
    }

    #[test]
    fn pragmatist_matches_documented_knob_values() {
        let p = pragmatist();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(tiers.review, Some(ModelTier::Local));
        assert_eq!(tiers.generate, Some(ModelTier::Opus));
        assert_eq!(tiers.docs, Some(ModelTier::Sonnet));
        assert_eq!(p.prompt_cache, Some(true));
        assert_eq!(p.review_mode, Some(ReviewMode::TrivialSkip));
        assert_eq!(p.llm_triage, Some(true));
        assert_eq!(p.test_depth, Some(TestDepth::Fast));
    }

    #[test]
    fn batch_reviewer_matches_documented_knob_values() {
        let p = batch_reviewer();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(tiers.generate, Some(ModelTier::Opus));
        assert_eq!(tiers.docs, Some(ModelTier::Sonnet));
        assert_eq!(p.review_mode, Some(ReviewMode::EndOnly));
        assert_eq!(p.test_depth, Some(TestDepth::Fast));
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.implement, Some(ModelTier::Sonnet));
        assert_eq!(tiers.review, Some(ModelTier::Sonnet));
        assert_eq!(tiers.triage, Some(ModelTier::Sonnet));
        assert_eq!(tiers.generate, Some(ModelTier::Opus));
        assert_eq!(tiers.docs, Some(ModelTier::Sonnet));
        assert_eq!(p.review_mode, Some(ReviewMode::PerTask));
        assert_eq!(p.llm_triage, Some(false));
        assert_eq!(p.test_depth, Some(TestDepth::Full));
    }

    /// CLAUDE.md standing rule 6: a knob absent from the profile bundles is
    /// a knob nobody will find. Both stages onboarded to the policy path
    /// must be pinned explicitly in every named bundle.
    #[test]
    fn every_named_profile_sets_the_generate_and_docs_tiers() {
        for name in ["baseline", "cheap-fast", "pragmatist", "batch-reviewer"] {
            let p = profile_by_name(name).expect("known profile name");
            let tiers = p.model_tiers.expect("model_tiers set");
            assert!(
                tiers.generate.is_some(),
                "profile `{name}` must set model_tiers.generate explicitly"
            );
            assert!(
                tiers.docs.is_some(),
                "profile `{name}` must set model_tiers.docs explicitly"
            );
        }
    }

    #[test]
    fn every_named_profile_sets_test_depth() {
        for name in ["baseline", "cheap-fast", "pragmatist", "batch-reviewer"] {
            let p = profile_by_name(name).expect("known profile name");
            assert!(
                p.test_depth.is_some(),
                "profile `{name}` must set test_depth explicitly"
            );
        }
    }
}
