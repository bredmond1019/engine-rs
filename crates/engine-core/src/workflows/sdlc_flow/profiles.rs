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
    ModelTier, OutputVerbosity, PartialCallTimeouts, PartialModelTiers, PartialPolicy,
    PartialRetryFeedback, PartialTransportRetry, ReviewMode, TestDepth,
};
use crate::policy::PartialLocalConfig;

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
        // Restates the built-in default verbatim — baseline's no-op contract.
        max_review_attempts: Some(3),
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
        // The cost/latency floor for the review loop too: fewer review
        // passes before the run bails to `WrapUpNode` rather than
        // continuing to spend on this profile's cheapest-and-fastest tiers.
        max_review_attempts: Some(2),
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
        // The middle setting — above `cheap-fast`'s floor, at the built-in
        // default, since `pragmatist` is not the profile trading away
        // review thoroughness.
        max_review_attempts: Some(3),
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
        // The quality ceiling: `end_only` collapses the per-task review
        // loop this knob bounds into one `EndReviewNode` call, so this
        // field does not gate anything on this profile today — but
        // standing rule 6 still requires it set explicitly, and the
        // quality-oriented bundle wants the full built-in default rather
        // than a truncated one.
        max_review_attempts: Some(3),
        ..Default::default()
    }
}

/// The quality ceiling rule 6 requires (`baseline` = explicit no-op,
/// `cheap-fast` = the cost/latency floor, `thorough` = this one): the
/// strongest tiers this workflow supports on every stage, full per-task
/// review, full test depth, the largest review-diff budget, and the full
/// retry/review budgets. Every field on [`PartialPolicy`] is set
/// explicitly — including `max_attempts` and `transport_retry`, which no
/// other named profile sets — so selecting `profile: "thorough"` is a
/// complete, self-documenting bundle rather than a partial one that falls
/// through to `harness.json`/built-in defaults for whatever it left out.
#[must_use]
pub fn thorough() -> PartialPolicy {
    PartialPolicy {
        output_verbosity: Some(OutputVerbosity::Verbose),
        prompt_cache: Some(true),
        review_mode: Some(ReviewMode::PerTask),
        // Not the trivial-skip/review-cutoff knobs' consumer path (review_mode
        // is per_task here, so these never gate a skip) — set explicitly
        // anyway, matching the built-in default, per rule 6's "every knob"
        // requirement for this bundle.
        review_skip_max_files: Some(2),
        review_skip_max_diff_lines: Some(40),
        test_depth: Some(TestDepth::Full),
        model_tiers: Some(PartialModelTiers {
            // The strongest tier this workflow supports, on every stage —
            // including `implement_simple`, which no production node reads
            // yet (see `policy.rs`'s note on the un-built simple-task path),
            // because rule 6 asks this bundle to set every knob explicitly,
            // not only the ones with a live consumer today.
            implement: Some(ModelTier::Opus),
            implement_simple: Some(ModelTier::Opus),
            review: Some(ModelTier::Opus),
            triage: Some(ModelTier::Opus),
            generate: Some(ModelTier::Opus),
            docs: Some(ModelTier::Opus),
        }),
        timeouts: Some(PartialCallTimeouts {
            // Generous per-stage ceiling rather than the built-in
            // unconfigured 300s: the quality ceiling favors letting a slow,
            // thorough call finish over cutting it off early. `implement`/
            // `triage`/`review` are the three stages `apply_policy` wires a
            // timeout to (see `SdlcPolicy::timeouts`'s doc comment).
            implement: Some(900),
            triage: Some(900),
            review: Some(900),
            // `generate` is declared-but-unread by `GenerateTasksNode`
            // (not yet onboarded to `apply_policy`) and `docs` is left
            // unset by every named profile in `harness.json` by
            // convention — see `repo_harness_json_deserializes_every_
            // sdlc_policy_and_profile`'s hard pin in `policy.rs`. Restated
            // as explicit `None` here, not omitted, so the choice reads as
            // deliberate rather than an oversight.
            generate: None,
            docs: None,
        }),
        local: Some(PartialLocalConfig {
            // Restates the built-in default verbatim (no stage above routes
            // to the `local` tier in this bundle) — set explicitly per rule
            // 6 rather than left `None`.
            endpoint: Some("http://localhost:11434".to_string()),
            model: Some("qwen2.5-coder:7b".to_string()),
            constrained_json: Some(false),
        }),
        llm_triage: Some(true),
        // The full task-retry budget — above the built-in default of 3,
        // the quality ceiling's willingness to keep retrying a task before
        // giving up on it.
        max_attempts: Some(5),
        // The full review-retry budget — same reasoning as max_attempts
        // above, kept as a separate counter (see `SdlcPolicy::max_review_
        // attempts`'s doc comment for why the two must never be conflated).
        max_review_attempts: Some(5),
        retry_feedback: Some(PartialRetryFeedback {
            enabled: Some(true),
            // Larger than every other profile's budget — the quality
            // ceiling wants more of the prior failure's evidence fed back,
            // not less.
            max_chars: Some(8_000),
        }),
        transport_retry: Some(PartialTransportRetry {
            // The full retry budget for a transient transport blip — more
            // attempts than the built-in default of 3, so a flaky call is
            // less likely to halt a thorough run outright.
            max_attempts: Some(5),
            initial_backoff_ms: Some(200),
        }),
        // The largest review-diff budget of any profile — above
        // `batch-reviewer`'s 200_000 ceiling — so the quality ceiling's
        // reviewer sees the most of a task's real diff before truncation.
        review_diff_max_chars: Some(400_000),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the canonical or additional profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        "pragmatist" => Some(pragmatist()),
        "batch-reviewer" => Some(batch_reviewer()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standing rule 6: `baseline`, `cheap-fast` and `thorough` are the
    /// three canonical profile names every workflow must ship. Renamed from
    /// `..._all_four_canonical_names` — that name is what let a missing
    /// `thorough` (which returned `None`) go unnoticed: the test asserted
    /// four real names and never checked that the *rule-6* three were among
    /// them.
    #[test]
    fn profile_by_name_resolves_the_three_canonical_rule6_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
    }

    /// `pragmatist` and `batch-reviewer` are real, additional profiles
    /// beyond the rule-6 three (rule 6 sets a floor on which profiles
    /// exist, not a ceiling) — kept resolving under their own test so a
    /// regression here is distinguishable from a rule-6 failure above.
    #[test]
    fn profile_by_name_resolves_the_additional_named_profiles() {
        assert_eq!(profile_by_name("pragmatist"), Some(pragmatist()));
        assert_eq!(profile_by_name("batch-reviewer"), Some(batch_reviewer()));
    }

    #[test]
    fn thorough_sets_every_partial_policy_field_explicitly() {
        let p = thorough();
        assert!(p.output_verbosity.is_some());
        assert!(p.prompt_cache.is_some());
        assert!(p.review_mode.is_some());
        assert!(p.review_skip_max_files.is_some());
        assert!(p.review_skip_max_diff_lines.is_some());
        assert!(p.test_depth.is_some());
        let tiers = p.model_tiers.as_ref().expect("model_tiers set");
        assert!(tiers.implement.is_some());
        assert!(tiers.implement_simple.is_some());
        assert!(tiers.review.is_some());
        assert!(tiers.triage.is_some());
        assert!(tiers.generate.is_some());
        assert!(tiers.docs.is_some());
        let timeouts = p.timeouts.as_ref().expect("timeouts set");
        assert!(timeouts.implement.is_some());
        assert!(timeouts.triage.is_some());
        assert!(timeouts.review.is_some());
        // `generate`/`docs` stay `None` deliberately — see `thorough`'s
        // doc comment: `generate` has no consumer yet and `docs` follows
        // the repo-wide harness.json convention every named profile keeps
        // (pinned by `repo_harness_json_deserializes_every_sdlc_policy_
        // and_profile` in `policy.rs`).
        assert!(timeouts.generate.is_none());
        assert!(timeouts.docs.is_none());
        let local = p.local.as_ref().expect("local set");
        assert!(local.endpoint.is_some());
        assert!(local.model.is_some());
        assert!(local.constrained_json.is_some());
        assert!(p.llm_triage.is_some());
        assert!(p.max_attempts.is_some());
        assert!(p.max_review_attempts.is_some());
        let retry_feedback = p.retry_feedback.as_ref().expect("retry_feedback set");
        assert!(retry_feedback.enabled.is_some());
        assert!(retry_feedback.max_chars.is_some());
        let transport_retry = p.transport_retry.as_ref().expect("transport_retry set");
        assert!(transport_retry.max_attempts.is_some());
        assert!(transport_retry.initial_backoff_ms.is_some());
        assert!(p.review_diff_max_chars.is_some());
    }

    #[test]
    fn thorough_uses_the_strongest_tier_on_every_stage() {
        let tiers = thorough().model_tiers.expect("model_tiers set");
        for tier in [
            tiers.implement,
            tiers.implement_simple,
            tiers.review,
            tiers.triage,
            tiers.generate,
            tiers.docs,
        ] {
            assert_eq!(tier, Some(ModelTier::Opus));
        }
    }

    #[test]
    fn thorough_has_the_largest_review_diff_budget_of_any_profile() {
        let thorough_chars = thorough().review_diff_max_chars.expect("set");
        for name in ["baseline", "cheap-fast", "pragmatist", "batch-reviewer"] {
            let other = profile_by_name(name)
                .expect("known profile name")
                .review_diff_max_chars
                .expect("set");
            assert!(
                thorough_chars > other,
                "thorough ({thorough_chars}) must exceed `{name}` ({other})"
            );
        }
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
        for name in [
            "baseline",
            "cheap-fast",
            "thorough",
            "pragmatist",
            "batch-reviewer",
        ] {
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
        for name in [
            "baseline",
            "cheap-fast",
            "thorough",
            "pragmatist",
            "batch-reviewer",
        ] {
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
            ("thorough", 400_000),
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

    /// Standing rule 6 for `max_review_attempts` (EN.ticket.review-retry-
    /// loop-unbounded task 1): every named profile must set it explicitly,
    /// and `baseline` must restate the built-in default verbatim.
    #[test]
    fn every_named_profile_sets_max_review_attempts() {
        let expected = [
            ("baseline", 3),
            ("cheap-fast", 2),
            ("pragmatist", 3),
            ("batch-reviewer", 3),
            ("thorough", 5),
        ];
        for (name, attempts) in expected {
            let p = profile_by_name(name).expect("known profile name");
            assert_eq!(
                p.max_review_attempts,
                Some(attempts),
                "profile `{name}` must set max_review_attempts explicitly"
            );
        }
        assert_eq!(
            baseline().max_review_attempts,
            Some(super::super::policy::SdlcPolicy::default().max_review_attempts),
            "baseline must restate the built-in default (its no-op contract)"
        );
    }
}
