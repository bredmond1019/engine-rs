//! Named `ProposalGeneratorPolicy` profile bundles, the
//! `proposal_generator.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run`.
//!
//! Mirrors `research_agent::profiles` (the named-bundle catalog +
//! `read_harness_policy_defaults`/`resolve_profile`/`resolve_policy_for_run`
//! trio), generalized over the shared `crate::policy::profiles` plumbing
//! (EN.4.0). There is no setup node in this workflow — every model node
//! calls [`resolve_policy_for_run`] directly.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{
    self, ModelTier, OutputVerbosity, PartialLocalConfig, PartialModelTiers,
    PartialProposalGeneratorPolicy, ProposalGeneratorPolicy, ReviewMode,
};
use super::schema::ProposalGeneratorEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`proposal_generator.policy` / `proposal_generator.profiles`),
/// passed to the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "proposal_generator";

/// The explicit control profile: all-Sonnet, normal verbosity, prompt cache
/// off, `review_mode = Full`. Spelled out explicitly (rather than left
/// all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialProposalGeneratorPolicy {
    PartialProposalGeneratorPolicy {
        output_verbosity: Some(OutputVerbosity::Normal),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Sonnet),
            opportunity: Some(ModelTier::Sonnet),
            writer: Some(ModelTier::Sonnet),
            review: Some(ModelTier::Sonnet),
            revise: Some(ModelTier::Sonnet),
        }),
        local: None,
        review_mode: Some(ReviewMode::Full),
    }
}

/// Cheapest/fastest judgment profile: `{opportunity, review, revise}` rewire
/// to `ModelTier::Local` with constrained JSON on; `research` (WebSearch)
/// and `writer` stay cloud-default (untouched — falls through to whatever
/// layer resolves them).
#[must_use]
pub fn local_judgment() -> PartialProposalGeneratorPolicy {
    PartialProposalGeneratorPolicy {
        output_verbosity: None,
        prompt_cache: None,
        model_tiers: Some(PartialModelTiers {
            research: None,
            opportunity: Some(ModelTier::Local),
            writer: None,
            review: Some(ModelTier::Local),
            revise: Some(ModelTier::Local),
        }),
        local: Some(PartialLocalConfig {
            endpoint: None,
            model: None,
            constrained_json: Some(true),
        }),
        review_mode: None,
    }
}

/// Fastest-to-persist profile: `review_mode = Skip` — `ProposalReviewNode`
/// short-circuits straight to `pass`, no revise branch ever taken. Other
/// knobs untouched (fall through to whatever layer resolves them).
#[must_use]
pub fn skip_review() -> PartialProposalGeneratorPolicy {
    PartialProposalGeneratorPolicy {
        output_verbosity: None,
        prompt_cache: None,
        model_tiers: None,
        local: None,
        review_mode: Some(ReviewMode::Skip),
    }
}

/// Cost/latency-floor profile (EN.14.J): all five stages rewire to
/// `ModelTier::Haiku`, terse output, prompt caching on, and the review loop
/// dropped to `Skip` — `ProposalReviewNode` short-circuits straight to
/// `pass`, no revise branch ever taken. Mirrors the shape of
/// `research_agent`'s `cheap_fast` — every existing knob stated, no new
/// knob introduced.
#[must_use]
pub fn cheap_fast() -> PartialProposalGeneratorPolicy {
    PartialProposalGeneratorPolicy {
        output_verbosity: Some(OutputVerbosity::Terse),
        prompt_cache: Some(true),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Haiku),
            opportunity: Some(ModelTier::Haiku),
            writer: Some(ModelTier::Haiku),
            review: Some(ModelTier::Haiku),
            revise: Some(ModelTier::Haiku),
        }),
        local: None,
        review_mode: Some(ReviewMode::Skip),
    }
}

/// Quality-ceiling profile (EN.14.J): all five stages rewire to
/// `ModelTier::Opus`, verbose output, prompt caching off, and the full
/// review loop (`ReviewMode::Full`) so every proposal is critiqued and
/// revised. Mirrors the shape of `research_agent`'s `thorough` — every
/// existing knob stated, no new knob introduced.
#[must_use]
pub fn thorough() -> PartialProposalGeneratorPolicy {
    PartialProposalGeneratorPolicy {
        output_verbosity: Some(OutputVerbosity::Verbose),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Opus),
            opportunity: Some(ModelTier::Opus),
            writer: Some(ModelTier::Opus),
            review: Some(ModelTier::Opus),
            revise: Some(ModelTier::Opus),
        }),
        local: None,
        review_mode: Some(ReviewMode::Full),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the five canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialProposalGeneratorPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        "local-judgment" => Some(local_judgment()),
        "skip-review" => Some(skip_review()),
        _ => None,
    }
}

/// Deserialize the inbound `PROPOSAL_GENERATOR` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ProposalGeneratorEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid PROPOSAL_GENERATOR event: {err}")))
}

/// Read `proposal_generator.policy` (a [`PartialProposalGeneratorPolicy`])
/// out of the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D), parameterized
/// by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialProposalGeneratorPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialProposalGeneratorPolicy`] bundle, preferring a `harness.json`
/// `proposal_generator.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries no
/// `profile` field, and `Err` when a `profile` name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialProposalGeneratorPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `proposal_generator.policy` section out of
/// a worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already had a
/// worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialProposalGeneratorPolicy>, NodeError> {
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
) -> Result<Option<PartialProposalGeneratorPolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`ProposalGeneratorPolicy`] for this run against
/// an arbitrary [`PolicyConfigSource`]: the inbound event's `policy`
/// override, the resolved `profile` bundle, `source`'s
/// `proposal_generator.policy` defaults, and the built-in default, high->low
/// precedence via [`policy::resolve`]. A [`PolicyConfigSource::Builtin`]
/// source resolves successfully with no filesystem access — the case a
/// worktree-free (channel/API-triggered) run needs.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<ProposalGeneratorPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    Ok(policy::resolve(
        ProposalGeneratorPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

/// Resolve the four-layer [`ProposalGeneratorPolicy`] for this run: the
/// inbound event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `proposal_generator.policy` defaults,
/// and the built-in default, high->low precedence via [`policy::resolve`].
/// This is what every model node calls — there is no dedicated setup node
/// in this workflow. Thin wrapper over [`resolve_policy_for_run_from`] with
/// [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<ProposalGeneratorPolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-proposal-generator-profiles-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn base_event() -> ProposalGeneratorEventSchema {
        ProposalGeneratorEventSchema {
            company_name: "Acme Corp".to_string(),
            company_url: None,
            diagnostic_intake: None,
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn base_ctx(event: ProposalGeneratorEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn profile_by_name_resolves_all_five_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
        assert_eq!(profile_by_name("local-judgment"), Some(local_judgment()));
        assert_eq!(profile_by_name("skip-review"), Some(skip_review()));
    }

    #[test]
    fn cheap_fast_resolves_all_five_stages_to_haiku_and_skips_review() {
        let resolved = policy::resolve(
            ProposalGeneratorPolicy::default(),
            None,
            Some(&cheap_fast()),
            None,
        );
        assert_eq!(resolved.model_tiers.research, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.writer, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.review, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Haiku);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        assert!(resolved.prompt_cache);
        assert_eq!(resolved.review_mode, ReviewMode::Skip);
    }

    #[test]
    fn thorough_resolves_all_five_stages_to_opus_and_keeps_full_review() {
        let resolved = policy::resolve(
            ProposalGeneratorPolicy::default(),
            None,
            Some(&thorough()),
            None,
        );
        assert_eq!(resolved.model_tiers.research, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.writer, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.review, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Opus);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Verbose);
        assert_eq!(resolved.review_mode, ReviewMode::Full);
    }

    #[test]
    fn resolve_policy_for_run_applies_named_cheap_fast_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("cheap-fast".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.research, ModelTier::Haiku);
        assert_eq!(resolved.review_mode, ReviewMode::Skip);
    }

    #[test]
    fn resolve_policy_for_run_applies_named_thorough_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("thorough".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.research, ModelTier::Opus);
        assert_eq!(resolved.review_mode, ReviewMode::Full);
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.research, Some(ModelTier::Sonnet));
        assert_eq!(tiers.opportunity, Some(ModelTier::Sonnet));
        assert_eq!(tiers.writer, Some(ModelTier::Sonnet));
        assert_eq!(tiers.review, Some(ModelTier::Sonnet));
        assert_eq!(tiers.revise, Some(ModelTier::Sonnet));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Normal));
        assert_eq!(p.prompt_cache, Some(false));
        assert_eq!(p.review_mode, Some(ReviewMode::Full));
    }

    #[test]
    fn local_judgment_resolves_opportunity_review_revise_to_local() {
        let resolved = policy::resolve(
            ProposalGeneratorPolicy::default(),
            None,
            Some(&local_judgment()),
            None,
        );
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Local);
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        // research never rewires via this profile.
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn skip_review_resolves_review_mode_to_skip() {
        let resolved = policy::resolve(
            ProposalGeneratorPolicy::default(),
            None,
            Some(&skip_review()),
            None,
        );
        assert_eq!(resolved.review_mode, ReviewMode::Skip);
        // Model tiers untouched by this profile.
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
    }

    #[test]
    fn harness_json_proposal_generator_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "proposal_generator": {
                    "policy": {
                        "output_verbosity": "terse",
                        "model_tiers": { "opportunity": "haiku" }
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "model_tiers": {
                                "research": "sonnet",
                                "opportunity": "sonnet",
                                "writer": "sonnet",
                                "review": "sonnet",
                                "revise": "sonnet"
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
        assert_eq!(defaults.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(
            defaults.model_tiers.unwrap().opportunity,
            Some(ModelTier::Haiku)
        );

        let resolved = resolve_profile(Some("baseline"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(
            resolved.model_tiers.unwrap().research,
            Some(ModelTier::Sonnet)
        );
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
        let result = resolve_profile(Some("local-judgment"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result, local_judgment());
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
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, ProposalGeneratorPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_local_judgment_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("local-judgment".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Local);
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
    }

    #[test]
    fn resolve_policy_for_run_applies_named_skip_review_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("skip-review".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.review_mode, ReviewMode::Skip);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "proposal_generator": {
                    "policy": { "model_tiers": { "review": "haiku" } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = base_event();
        event.policy = Some(PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                review: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.review, ModelTier::Opus);
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

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, ProposalGeneratorPolicy::default());
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
    fn resolve_policy_for_run_from_harness_file_source_preserves_precedence() {
        let dir = temp_dir();
        let harness_file = dir.join("standalone-harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "proposal_generator": { "policy": { "model_tiers": { "review": "haiku" } } }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let mut event = base_event();
        event.policy = Some(PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                review: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.model_tiers.review, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("local-judgment".to_string());
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }
}
