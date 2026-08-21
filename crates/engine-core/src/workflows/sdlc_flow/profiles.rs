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
        // Restates the built-in default verbatim — baseline's no-op contract.
        review_diff_max_chars: Some(120_000),
        ..Default::default()
    }
}

/// Cheapest/fastest profile: `haiku` on every stage it pins, terse
/// output, trivial-task review skip, `test_depth: fast` (the cost/latency
/// floor — per-task check selection is the single largest lever in this
/// repo, per CLAUDE.md's measured 2m44s -> 6.4s).
///
/// `triage` and `review` were `local` until 2026-08-01. They are cloud
/// (`haiku`) now because the `local` tier's default Ollama model is not
/// pulled on every machine that runs this workflow, and a missing model is
/// not a graceful degradation — `ConsolidatedReviewNode` died on a live run
/// with `HTTP 404 ... selected model (qwen2.5:3b) ... may not exist`.
/// `haiku` keeps this profile's cost floor intact. Routing these stages back
/// to `local` is a deliberate future revisit, gated on the local models
/// actually being provisioned.
#[must_use]
pub fn cheap_fast() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Haiku),
            triage: Some(ModelTier::Haiku),
            review: Some(ModelTier::Haiku),
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
        // The cost/latency floor for the reviewer prompt — a spend choice,
        // not a capacity limit. This number was originally sized for a
        // LOCAL reviewer's context window; now that `review` is `haiku`,
        // 20_000 chars is conservative for the window actually available.
        // Kept unchanged deliberately: it is still a defensible floor for
        // the cheapest profile, and re-tuning it is its own follow-up.
        review_diff_max_chars: Some(20_000),
        ..Default::default()
    }
}

/// Balanced profile: `sonnet` implement, `sonnet` review, prompt caching on,
/// trivial-task review skip, `llm_triage` on, `test_depth: fast`.
///
/// `review` was `local` until 2026-08-01; it is `sonnet` now for the same
/// reason `cheap-fast` moved to `haiku` (the local tier's model is not
/// provisioned on every machine, and its absence is a hard HTTP 404 inside
/// `ConsolidatedReviewNode`, not a degradation). `sonnet` matches this
/// profile's `implement` tier, keeping its balance intact.
#[must_use]
pub fn pragmatist() -> PartialPolicy {
    PartialPolicy {
        model_tiers: Some(PartialModelTiers {
            implement: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Sonnet),
            generate: Some(ModelTier::Opus),
            docs: Some(ModelTier::Sonnet),
            ..Default::default()
        }),
        prompt_cache: Some(true),
        review_mode: Some(ReviewMode::TrivialSkip),
        llm_triage: Some(true),
        test_depth: Some(TestDepth::Fast),
        // Above `cheap-fast`, below `batch-reviewer`'s ceiling — a middle
        // spend/latency setting, not a capacity limit. Like `cheap-fast`'s
        // 20_000, this number was sized when `review` ran on the `local`
        // tier and is therefore conservative for the Sonnet window it now
        // has. Left unchanged on purpose; re-tuning is a separate follow-up.
        review_diff_max_chars: Some(40_000),
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
        // The quality ceiling. `end_only` makes `EndReviewNode` (the drain
        // branch's single review node, `end_review.rs`) issue exactly ONE
        // review call that sees the WHOLE run's accumulated diff rather
        // than one task's, and it sees it on Sonnet — the profile with
        // both the largest input and the most room for it. This is
        // single-pass: unlike JS's end review, there is no fix loop, so a
        // FAIL verdict blocks the run rather than retrying it (see
        // `EndReviewRouterNode`'s doc comment for why). Proven by
        // `end_only_full_run_makes_exactly_one_review_call_with_full_ac_and_multi_task_diff`
        // in `crates/engine-core/tests/it/sdlc_flow_end_review_e2e.rs`.
        review_diff_max_chars: Some(200_000),
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
        // Cloud, not `Local` — see `cheap_fast`'s doc comment (2026-08-01).
        assert_eq!(tiers.triage, Some(ModelTier::Haiku));
        assert_eq!(tiers.review, Some(ModelTier::Haiku));
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
        // Cloud, not `Local` — see `pragmatist`'s doc comment (2026-08-01).
        assert_eq!(tiers.review, Some(ModelTier::Sonnet));
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

    /// Standing rule 6 again, for the reviewer-diff bound. Pinned with the
    /// exact ordering the profiles are tuned to — `cheap-fast` at the floor,
    /// `batch-reviewer` at the ceiling, `baseline` restating the built-in
    /// default — so a careless edit that flattens them fails here.
    #[test]
    fn every_named_profile_sets_review_diff_max_chars() {
        let expected = [
            ("baseline", 120_000),
            ("cheap-fast", 20_000),
            ("pragmatist", 40_000),
            ("batch-reviewer", 200_000),
        ];
        for (name, chars) in expected {
            let p = profile_by_name(name).expect("known profile name");
            assert_eq!(
                p.review_diff_max_chars,
                Some(chars),
                "profile `{name}` must set review_diff_max_chars explicitly"
            );
        }
        assert_eq!(
            baseline().review_diff_max_chars,
            Some(super::super::policy::SdlcPolicy::default().review_diff_max_chars),
            "baseline must restate the built-in default (its no-op contract)"
        );
    }
}
