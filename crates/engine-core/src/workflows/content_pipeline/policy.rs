//! `ContentPipelinePolicy` / `PartialContentPipelinePolicy` and the
//! `Policy` trait delegation to `crate::policy::resolve::resolve`.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `content_pipeline.policy` defaults, then
//! built-in defaults. The built-in [`ContentPipelinePolicy::default`] is a
//! documented, behavior-stable baseline (all-Sonnet, normal verbosity,
//! prompt cache off, `max_critic_iterations = 3`,
//! `critic_confidence_threshold = 0.8`) across the four model stages.
//!
//! Four stages: `summarize` (`SummarizeNode`), `critic` (`SelfCriticNode`),
//! `revise` (`ReviseNode`), `translate` (`TranslateNode`) — all
//! Local-eligible per `graph::registry_for_policy`. The fetch/normalize/
//! render/persist stages have no `ModelTier` field and never rewire to
//! Local. `EN.6.A` adds a fifth, non-model `dispatch` stage
//! (`ActionDispatchNode`): `dispatch_verbosity` is telemetry/verbosity
//! config only — there is no `dispatch` entry in `ModelTiers`, no rewire
//! branch in `graph::registry_for_policy`, and it is never Local-eligible.
//!
//! Built on `EN.5.D`'s derived [`crate::policy::Overlay`] — this module does
//! not hand-write another `merge_opt`/`merge_local`/`apply_override` trio.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

/// Hard ceiling on `max_critic_iterations` across every override layer.
/// A caller-supplied value above this (via `harness.json`, a named
/// profile, or a per-event override) is rejected by
/// [`validate_bounds`] rather than silently clamped or accepted — see
/// `profiles::resolve_policy_for_run_from`.
pub const MAX_CRITIC_ITERATIONS_CEILING: u32 = 10;

/// Per-stage model tier assignment for the four Local-eligible nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub summarize: ModelTier,
    pub critic: ModelTier,
    pub revise: ModelTier,
    pub translate: ModelTier,
}

impl Default for ModelTiers {
    /// All-Sonnet — the behavior-stable baseline.
    fn default() -> Self {
        Self {
            summarize: ModelTier::Sonnet,
            critic: ModelTier::Sonnet,
            revise: ModelTier::Sonnet,
            translate: ModelTier::Sonnet,
        }
    }
}

/// The fully-resolved, per-run Content Pipeline policy — the merge of
/// built-in defaults, `harness.json`'s `content_pipeline.policy` defaults,
/// a named `profile`, and any per-run event override, high->low precedence
/// in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContentPipelinePolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub model_tiers: ModelTiers,
    /// Configuration for the `local` model tier — used by whichever of
    /// `{summarize, critic, revise, translate}` resolves to
    /// `ModelTier::Local`.
    pub local: LocalConfig,
    /// Bounded self-critic loop cap. `iteration` counts revisions
    /// completed, so `max_critic_iterations = N` yields exactly N critic
    /// passes (see `critic_router.rs` / architecture.md §4).
    pub max_critic_iterations: u32,
    /// Confidence exit threshold in `[0, 1]`; the loop also exits early
    /// once `CriticEvaluation.confidence` meets or exceeds this value.
    pub critic_confidence_threshold: f64,
    /// Verbosity for the non-model `dispatch` stage (`ActionDispatchNode`,
    /// `EN.6.A`) — telemetry/logging shaping only. This is deliberately
    /// **not** a `ModelTier`: the dispatch stage is a deterministic egress
    /// node, never Local-eligible, and `graph::registry_for_policy` has no
    /// rewire branch for it.
    pub dispatch_verbosity: OutputVerbosity,
}

impl Default for ContentPipelinePolicy {
    /// The safe default: normal verbosity, all-Sonnet tiers, prompt-cache
    /// off, 3 critic iterations max, 0.8 confidence exit threshold.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            model_tiers: ModelTiers::default(),
            local: LocalConfig::default(),
            max_critic_iterations: 3,
            critic_confidence_threshold: 0.8,
            dispatch_verbosity: OutputVerbosity::Normal,
        }
    }
}

/// All-optional mirror of [`ModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub summarize: Option<ModelTier>,
    pub critic: Option<ModelTier>,
    pub revise: Option<ModelTier>,
    pub translate: Option<ModelTier>,
}

/// All-optional mirror of [`ContentPipelinePolicy`] used by the override
/// layers (`harness.json`'s `content_pipeline.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialContentPipelinePolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
    pub max_critic_iterations: Option<u32>,
    pub critic_confidence_threshold: Option<f64>,
    pub dispatch_verbosity: Option<OutputVerbosity>,
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.summarize {
        base.summarize = v;
    }
    if let Some(v) = over.critic {
        base.critic = v;
    }
    if let Some(v) = over.revise {
        base.revise = v;
    }
    if let Some(v) = over.translate {
        base.translate = v;
    }
    base
}

impl crate::policy::Policy for ContentPipelinePolicy {
    type Partial = PartialContentPipelinePolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialContentPipelinePolicy) -> Self {
        let base = self;
        ContentPipelinePolicy {
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
            max_critic_iterations: merge_opt(
                base.max_critic_iterations,
                over.max_critic_iterations,
            ),
            critic_confidence_threshold: merge_opt(
                base.critic_confidence_threshold,
                over.critic_confidence_threshold,
            ),
            dispatch_verbosity: merge_opt(base.dispatch_verbosity, over.dispatch_verbosity),
        }
    }
}

/// Resolve the four policy layers into one concrete
/// [`ContentPipelinePolicy`], high->low precedence: `event_override` beats
/// `profile` beats `harness_defaults` beats `builtin`. Delegates to the
/// generic `crate::policy::resolve::resolve` now that
/// `ContentPipelinePolicy` implements `crate::policy::Policy`.
///
/// This does **not** validate the resolved loop bounds — callers must run
/// [`validate_bounds`] on the result (see
/// `profiles::resolve_policy_for_run_from`, which does exactly that before
/// returning).
///
/// `builtin` is normally `ContentPipelinePolicy::default()`, taken as a
/// parameter so callers/tests can exercise the merge against any base.
/// `profile` is a named bundle (see `profiles::profile_by_name` and
/// `harness.json`'s `content_pipeline.profiles`) resolved by the caller
/// before this function runs.
#[must_use]
pub fn resolve(
    builtin: ContentPipelinePolicy,
    harness_defaults: Option<&PartialContentPipelinePolicy>,
    profile: Option<&PartialContentPipelinePolicy>,
    event_override: Option<&PartialContentPipelinePolicy>,
) -> ContentPipelinePolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

/// Validate a resolved policy's caller-tunable self-critic loop bounds.
/// Returns `Err` naming the offending bound rather than silently clamping
/// or accepting it — an out-of-range `max_critic_iterations` (above
/// [`MAX_CRITIC_ITERATIONS_CEILING`] or zero) or a
/// `critic_confidence_threshold` outside `[0, 1]` is a rejected override,
/// not a rounded one.
pub fn validate_bounds(policy: &ContentPipelinePolicy) -> Result<(), String> {
    if policy.max_critic_iterations == 0
        || policy.max_critic_iterations > MAX_CRITIC_ITERATIONS_CEILING
    {
        return Err(format!(
            "max_critic_iterations must be in 1..={MAX_CRITIC_ITERATIONS_CEILING}, got {}",
            policy.max_critic_iterations
        ));
    }
    if !(0.0..=1.0).contains(&policy.critic_confidence_threshold) {
        return Err(format!(
            "critic_confidence_threshold must be in [0, 1], got {}",
            policy.critic_confidence_threshold
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ContentPipelinePolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.model_tiers.summarize, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.critic, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.revise, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.translate, ModelTier::Sonnet);
        assert!(!policy.prompt_cache);
        assert_eq!(policy.max_critic_iterations, 3);
        assert!((policy.critic_confidence_threshold - 0.8).abs() < f64::EPSILON);
        assert_eq!(policy.dispatch_verbosity, OutputVerbosity::Normal);
    }

    #[test]
    fn dispatch_verbosity_overrides_independently_of_model_tiers() {
        let event = PartialContentPipelinePolicy {
            dispatch_verbosity: Some(OutputVerbosity::Verbose),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.dispatch_verbosity, OutputVerbosity::Verbose);
        // Untouched model tiers still fall through to builtin.
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Sonnet);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, None);
        assert_eq!(resolved, ContentPipelinePolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialContentPipelinePolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Sonnet);
    }

    #[test]
    fn profile_beats_harness_defaults_for_model_tiers_stage() {
        let harness = PartialContentPipelinePolicy {
            model_tiers: Some(PartialModelTiers {
                critic: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialContentPipelinePolicy {
            model_tiers: Some(PartialModelTiers {
                critic: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ContentPipelinePolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        // Other stage untouched by either override falls through to builtin.
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Sonnet);
    }

    #[test]
    fn event_override_beats_profile_for_model_tiers_stage() {
        let profile = PartialContentPipelinePolicy {
            model_tiers: Some(PartialModelTiers {
                revise: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialContentPipelinePolicy {
            model_tiers: Some(PartialModelTiers {
                revise: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ContentPipelinePolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
    }

    #[test]
    fn local_overrides_survive_on_all_four_stages() {
        let event = PartialContentPipelinePolicy {
            model_tiers: Some(PartialModelTiers {
                summarize: Some(ModelTier::Local),
                critic: Some(ModelTier::Local),
                revise: Some(ModelTier::Local),
                translate: Some(ModelTier::Local),
            }),
            local: Some(PartialLocalConfig {
                endpoint: Some("http://localhost:8080".to_string()),
                model: Some("local-model".to_string()),
                constrained_json: Some(true),
            }),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Local);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Local);
        assert_eq!(resolved.local.endpoint, "http://localhost:8080");
        assert_eq!(resolved.local.model, "local-model");
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn loop_bounds_overridable_per_event() {
        let event = PartialContentPipelinePolicy {
            max_critic_iterations: Some(5),
            critic_confidence_threshold: Some(0.95),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.max_critic_iterations, 5);
        assert!((resolved.critic_confidence_threshold - 0.95).abs() < f64::EPSILON);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "prompt_cache": true,
            "model_tiers": { "summarize": "local", "critic": "local", "revise": "local", "translate": "local" },
            "max_critic_iterations": 2,
            "critic_confidence_threshold": 0.9
        }"#;
        let partial: PartialContentPipelinePolicy =
            serde_json::from_str(json).expect("valid PartialContentPipelinePolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.prompt_cache, Some(true));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().summarize,
            Some(ModelTier::Local)
        );
        assert_eq!(partial.max_critic_iterations, Some(2));
        assert_eq!(partial.critic_confidence_threshold, Some(0.9));
        // Not present in the JSON -> None on the local mirror.
        assert_eq!(partial.local, None);
    }

    #[test]
    fn validate_bounds_accepts_the_builtin_default() {
        assert!(validate_bounds(&ContentPipelinePolicy::default()).is_ok());
    }

    #[test]
    fn validate_bounds_rejects_zero_max_critic_iterations() {
        let policy = ContentPipelinePolicy {
            max_critic_iterations: 0,
            ..ContentPipelinePolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject zero");
        assert!(err.contains("max_critic_iterations"));
    }

    #[test]
    fn validate_bounds_rejects_max_critic_iterations_above_ceiling() {
        let policy = ContentPipelinePolicy {
            max_critic_iterations: MAX_CRITIC_ITERATIONS_CEILING + 1,
            ..ContentPipelinePolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject above ceiling");
        assert!(err.contains("max_critic_iterations"));
    }

    #[test]
    fn validate_bounds_accepts_max_critic_iterations_at_ceiling() {
        let policy = ContentPipelinePolicy {
            max_critic_iterations: MAX_CRITIC_ITERATIONS_CEILING,
            ..ContentPipelinePolicy::default()
        };
        assert!(validate_bounds(&policy).is_ok());
    }

    #[test]
    fn validate_bounds_rejects_confidence_threshold_below_zero() {
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: -0.01,
            ..ContentPipelinePolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject below zero");
        assert!(err.contains("critic_confidence_threshold"));
    }

    #[test]
    fn validate_bounds_rejects_confidence_threshold_above_one() {
        let policy = ContentPipelinePolicy {
            critic_confidence_threshold: 1.01,
            ..ContentPipelinePolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject above one");
        assert!(err.contains("critic_confidence_threshold"));
    }

    #[test]
    fn validate_bounds_accepts_boundary_confidence_thresholds() {
        let low = ContentPipelinePolicy {
            critic_confidence_threshold: 0.0,
            ..ContentPipelinePolicy::default()
        };
        assert!(validate_bounds(&low).is_ok());
        let high = ContentPipelinePolicy {
            critic_confidence_threshold: 1.0,
            ..ContentPipelinePolicy::default()
        };
        assert!(validate_bounds(&high).is_ok());
    }
}
