//! `ProposalGeneratorPolicy` / `PartialProposalGeneratorPolicy` and the
//! `Policy` trait delegation to `crate::policy::resolve::resolve`.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `proposal_generator.policy` defaults, then
//! built-in defaults. The built-in [`ProposalGeneratorPolicy::default`] is
//! a documented, behavior-stable baseline (all-Sonnet, normal verbosity,
//! prompt cache off, `review_mode = Full`) across all five stages.
//!
//! Five stages: `research` (WebSearch — cloud-only, `ProposalCompanyResearchNode`),
//! `opportunity` (Local-eligible, `OpportunityIdentifierNode`), `writer`
//! (cloud-default, `ProposalWriterNode`), `review` (Local-eligible,
//! `ProposalReviewNode`), `revise` (Local-eligible, `ProposalReviseNode`).
//! `research` never resolves to `ModelTier::Local` — it wraps `ClaudeCodeStep`
//! with WebSearch/WebFetch tools granted, which a local single-shot endpoint
//! cannot serve.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

/// Whether `ProposalReviewNode` runs a real model judgment or short-circuits
/// to a `pass` verdict without a model call.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    /// Run the review model call and honor its verdict.
    #[default]
    Full,
    /// Short-circuit straight to `pass` — no model call, no revise branch.
    Skip,
}

/// Per-stage model tier assignment for the five nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposalGeneratorModelTiers {
    pub research: ModelTier,
    pub opportunity: ModelTier,
    pub writer: ModelTier,
    pub review: ModelTier,
    pub revise: ModelTier,
}

impl Default for ProposalGeneratorModelTiers {
    /// All-Sonnet — the behavior-stable baseline.
    fn default() -> Self {
        Self {
            research: ModelTier::Sonnet,
            opportunity: ModelTier::Sonnet,
            writer: ModelTier::Sonnet,
            review: ModelTier::Sonnet,
            revise: ModelTier::Sonnet,
        }
    }
}

/// The fully-resolved, per-run Proposal Generator policy — the merge of
/// built-in defaults, `harness.json`'s `proposal_generator.policy` defaults,
/// a named `profile`, and any per-run event override, high->low precedence
/// in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProposalGeneratorPolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub model_tiers: ProposalGeneratorModelTiers,
    /// Configuration for the `local` model tier — used by whichever of
    /// `{opportunity, review, revise}` resolves to `ModelTier::Local`;
    /// `research` never rewires to it.
    pub local: LocalConfig,
    /// Domain knob: whether `ProposalReviewNode` actually runs.
    pub review_mode: ReviewMode,
}

impl Default for ProposalGeneratorPolicy {
    /// The safe default: normal verbosity, all-Sonnet tiers, prompt-cache
    /// off, `review_mode = Full`.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            model_tiers: ProposalGeneratorModelTiers::default(),
            local: LocalConfig::default(),
            review_mode: ReviewMode::default(),
        }
    }
}

/// All-optional mirror of [`ProposalGeneratorModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub research: Option<ModelTier>,
    pub opportunity: Option<ModelTier>,
    pub writer: Option<ModelTier>,
    pub review: Option<ModelTier>,
    pub revise: Option<ModelTier>,
}

/// All-optional mirror of [`ProposalGeneratorPolicy`] used by the override
/// layers (`harness.json`'s `proposal_generator.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialProposalGeneratorPolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
    pub review_mode: Option<ReviewMode>,
}

fn merge_model_tiers(
    mut base: ProposalGeneratorModelTiers,
    over: &PartialModelTiers,
) -> ProposalGeneratorModelTiers {
    if let Some(v) = over.research {
        base.research = v;
    }
    if let Some(v) = over.opportunity {
        base.opportunity = v;
    }
    if let Some(v) = over.writer {
        base.writer = v;
    }
    if let Some(v) = over.review {
        base.review = v;
    }
    if let Some(v) = over.revise {
        base.revise = v;
    }
    base
}

impl crate::policy::Policy for ProposalGeneratorPolicy {
    type Partial = PartialProposalGeneratorPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialProposalGeneratorPolicy) -> Self {
        let base = self;
        ProposalGeneratorPolicy {
            output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
            prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
            model_tiers: match &over.model_tiers {
                Some(mt) => merge_model_tiers(base.model_tiers, mt),
                None => base.model_tiers,
            },
            local: match &over.local {
                Some(l) => base.local.overlay(l),
                None => base.local,
            },
            review_mode: merge_opt(base.review_mode, over.review_mode),
        }
    }
}

/// Resolve the four policy layers into one concrete
/// [`ProposalGeneratorPolicy`], high->low precedence: `event_override` beats
/// `profile` beats `harness_defaults` beats `builtin`. Delegates to the
/// generic `crate::policy::resolve::resolve` now that
/// `ProposalGeneratorPolicy` implements `crate::policy::Policy`.
///
/// `builtin` is normally `ProposalGeneratorPolicy::default()`, taken as a
/// parameter so callers/tests can exercise the merge against any base.
/// `profile` is a named bundle (see `profiles::profile_by_name` and
/// `harness.json`'s `proposal_generator.profiles`) resolved by the caller
/// before this function runs.
#[must_use]
pub fn resolve(
    builtin: ProposalGeneratorPolicy,
    harness_defaults: Option<&PartialProposalGeneratorPolicy>,
    profile: Option<&PartialProposalGeneratorPolicy>,
    event_override: Option<&PartialProposalGeneratorPolicy>,
) -> ProposalGeneratorPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ProposalGeneratorPolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.model_tiers.research, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.opportunity, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.writer, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.review, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.revise, ModelTier::Sonnet);
        assert!(!policy.prompt_cache);
        assert_eq!(policy.review_mode, ReviewMode::Full);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(ProposalGeneratorPolicy::default(), None, None, None);
        assert_eq!(resolved, ProposalGeneratorPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialProposalGeneratorPolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(
            ProposalGeneratorPolicy::default(),
            Some(&harness),
            None,
            None,
        );
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
    }

    #[test]
    fn profile_beats_harness_defaults_for_model_tiers_stage() {
        let harness = PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                opportunity: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                opportunity: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ProposalGeneratorPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Local);
        // Other stage untouched by either override falls through to builtin.
        assert_eq!(resolved.model_tiers.writer, ModelTier::Sonnet);
    }

    #[test]
    fn event_override_beats_profile_for_model_tiers_stage() {
        let profile = PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                review: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                review: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ProposalGeneratorPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
    }

    #[test]
    fn local_overrides_survive_on_opportunity_review_revise() {
        let event = PartialProposalGeneratorPolicy {
            model_tiers: Some(PartialModelTiers {
                opportunity: Some(ModelTier::Local),
                review: Some(ModelTier::Local),
                revise: Some(ModelTier::Local),
                ..Default::default()
            }),
            local: Some(PartialLocalConfig {
                endpoint: Some("http://localhost:8080".to_string()),
                model: Some("local-model".to_string()),
                constrained_json: Some(true),
            }),
            ..Default::default()
        };
        let resolved = resolve(ProposalGeneratorPolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.model_tiers.opportunity, ModelTier::Local);
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        // research never resolves to Local via this override (not set).
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
        assert_eq!(resolved.local.endpoint, "http://localhost:8080");
        assert_eq!(resolved.local.model, "local-model");
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn review_mode_override_resolves() {
        let event = PartialProposalGeneratorPolicy {
            review_mode: Some(ReviewMode::Skip),
            ..Default::default()
        };
        let resolved = resolve(ProposalGeneratorPolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.review_mode, ReviewMode::Skip);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "prompt_cache": true,
            "model_tiers": { "opportunity": "local", "review": "local", "revise": "local" },
            "review_mode": "skip"
        }"#;
        let partial: PartialProposalGeneratorPolicy =
            serde_json::from_str(json).expect("valid PartialProposalGeneratorPolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.prompt_cache, Some(true));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().opportunity,
            Some(ModelTier::Local)
        );
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().review,
            Some(ModelTier::Local)
        );
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().revise,
            Some(ModelTier::Local)
        );
        assert_eq!(partial.review_mode, Some(ReviewMode::Skip));
        // Not present in the JSON -> None on the local mirror.
        assert_eq!(partial.local, None);
    }
}
