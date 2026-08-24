//! `LinkedInPostPolicy` / `PartialLinkedInPostPolicy` and the `Policy`
//! trait delegation to `crate::policy::resolve::resolve`.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `linkedin_post.policy` defaults, then built-in
//! defaults. The built-in [`LinkedInPostPolicy::default`] is a documented,
//! behavior-stable baseline (all-Sonnet, `max_critic_iterations = 3`,
//! `candidate_count = 3`, translate on).
//!
//! Per `planning/EN.5.G/tasks.md` + `tasks.json` task 3, the knobs are:
//! model tier per stage (`draft` / `critic` / `translate`), the critic
//! revise-loop iteration cap, `candidate_count`, and whether the PT
//! `TranslateNode` pass runs at all. Mirrors
//! `content_pipeline::policy`/`profiles`, generalized over the shared
//! `crate::policy` plumbing (EN.4.0) — this module does not hand-write
//! another `merge_opt`/`Overlay`/`resolve` trio.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

/// Hard ceiling on `max_critic_iterations` across every override layer. A
/// caller-supplied value above this (via `harness.json`, a named profile,
/// or a per-event override) is rejected by [`validate_bounds`] rather than
/// silently clamped or accepted — see `profiles::resolve_policy_for_run_from`.
pub const MAX_CRITIC_ITERATIONS_CEILING: u32 = 10;

/// Hard ceiling on `candidate_count` across every override layer. Same
/// reject-not-clamp discipline as [`MAX_CRITIC_ITERATIONS_CEILING`].
pub const MAX_CANDIDATE_COUNT_CEILING: u32 = 20;

/// Per-stage model tier assignment for the three Local-eligible nodes:
/// `draft` (`PostDraftNode`), `critic` (`BrandCriticNode`), `translate`
/// (`TranslateNode`, reused from `content_pipeline`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub draft: ModelTier,
    pub critic: ModelTier,
    pub translate: ModelTier,
}

impl Default for ModelTiers {
    /// All-Sonnet — the behavior-stable baseline.
    fn default() -> Self {
        Self {
            draft: ModelTier::Sonnet,
            critic: ModelTier::Sonnet,
            translate: ModelTier::Sonnet,
        }
    }
}

/// The fully-resolved, per-run `LINKEDIN_POST` policy — the merge of
/// built-in defaults, `harness.json`'s `linkedin_post.policy` defaults, a
/// named `profile`, and any per-run event override, high->low precedence
/// in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LinkedInPostPolicy {
    pub model_tiers: ModelTiers,
    /// Configuration for the `local` model tier — used by whichever of
    /// `{draft, critic, translate}` resolves to `ModelTier::Local`.
    pub local: LocalConfig,
    /// Bounded brand-critic revise loop cap (`critic_router.rs` /
    /// `IncrementCriticIterationNode`), same shape as
    /// `content_pipeline`'s.
    pub max_critic_iterations: u32,
    /// How many `PostCandidate`s `PostDraftNode` proposes. Distinct from
    /// `LinkedInPostEventSchema::candidate_count`: the event field is the
    /// per-run request, this is the policy-layer default a run falls back
    /// on when the event omits it AND a knob a profile/harness default can
    /// steer independently of the caller.
    pub candidate_count: u32,
    /// Whether the PT `TranslateNode` pass runs at all. `false` routes
    /// `TranslateNode` to its no-op path — the node stays in the declared
    /// graph (standing rule 6: policy must not rewire the node set), it
    /// simply skips producing a translation.
    pub translate_enabled: bool,
}

impl Default for LinkedInPostPolicy {
    /// The safe default: all-Sonnet tiers, 3 critic iterations max, 3
    /// candidates, translate on.
    fn default() -> Self {
        Self {
            model_tiers: ModelTiers::default(),
            local: LocalConfig::default(),
            max_critic_iterations: 3,
            candidate_count: 3,
            translate_enabled: true,
        }
    }
}

/// All-optional mirror of [`ModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub draft: Option<ModelTier>,
    pub critic: Option<ModelTier>,
    pub translate: Option<ModelTier>,
}

/// All-optional mirror of [`LinkedInPostPolicy`] used by the override
/// layers (`harness.json`'s `linkedin_post.policy`, a named `profile`, and
/// a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialLinkedInPostPolicy {
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
    pub max_critic_iterations: Option<u32>,
    pub candidate_count: Option<u32>,
    pub translate_enabled: Option<bool>,
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.draft {
        base.draft = v;
    }
    if let Some(v) = over.critic {
        base.critic = v;
    }
    if let Some(v) = over.translate {
        base.translate = v;
    }
    base
}

impl crate::policy::Policy for LinkedInPostPolicy {
    type Partial = PartialLinkedInPostPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some`
    /// in `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialLinkedInPostPolicy) -> Self {
        let base = self;
        LinkedInPostPolicy {
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
            candidate_count: merge_opt(base.candidate_count, over.candidate_count),
            translate_enabled: merge_opt(base.translate_enabled, over.translate_enabled),
        }
    }
}

/// Resolve the four policy layers into one concrete [`LinkedInPostPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats `builtin`. Delegates to the generic
/// `crate::policy::resolve::resolve` now that `LinkedInPostPolicy`
/// implements `crate::policy::Policy`.
///
/// This does **not** validate the resolved loop bounds — callers must run
/// [`validate_bounds`] on the result (see
/// `profiles::resolve_policy_for_run_from`, which does exactly that before
/// returning).
#[must_use]
pub fn resolve(
    builtin: LinkedInPostPolicy,
    harness_defaults: Option<&PartialLinkedInPostPolicy>,
    profile: Option<&PartialLinkedInPostPolicy>,
    event_override: Option<&PartialLinkedInPostPolicy>,
) -> LinkedInPostPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

/// Validate a resolved policy's caller-tunable bounds. Returns `Err`
/// naming the offending bound rather than silently clamping or accepting
/// it — an out-of-range `max_critic_iterations` (above
/// [`MAX_CRITIC_ITERATIONS_CEILING`] or zero) or `candidate_count` (above
/// [`MAX_CANDIDATE_COUNT_CEILING`] or zero) is a rejected override, not a
/// rounded one.
pub fn validate_bounds(policy: &LinkedInPostPolicy) -> Result<(), String> {
    if policy.max_critic_iterations == 0
        || policy.max_critic_iterations > MAX_CRITIC_ITERATIONS_CEILING
    {
        return Err(format!(
            "max_critic_iterations must be in 1..={MAX_CRITIC_ITERATIONS_CEILING}, got {}",
            policy.max_critic_iterations
        ));
    }
    if policy.candidate_count == 0 || policy.candidate_count > MAX_CANDIDATE_COUNT_CEILING {
        return Err(format!(
            "candidate_count must be in 1..={MAX_CANDIDATE_COUNT_CEILING}, got {}",
            policy.candidate_count
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = LinkedInPostPolicy::default();
        assert_eq!(policy.model_tiers.draft, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.critic, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.translate, ModelTier::Sonnet);
        assert_eq!(policy.max_critic_iterations, 3);
        assert_eq!(policy.candidate_count, 3);
        assert!(policy.translate_enabled);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(LinkedInPostPolicy::default(), None, None, None);
        assert_eq!(resolved, LinkedInPostPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_draft_tier() {
        let harness = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                draft: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(LinkedInPostPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.model_tiers.draft, ModelTier::Haiku);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.critic, ModelTier::Sonnet);
    }

    #[test]
    fn profile_beats_harness_defaults_for_critic_tier() {
        let harness = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                critic: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                critic: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            LinkedInPostPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        assert_eq!(resolved.model_tiers.draft, ModelTier::Sonnet);
    }

    #[test]
    fn event_override_beats_profile_for_translate_tier() {
        let profile = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                translate: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                translate: Some(ModelTier::Local),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            LinkedInPostPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.translate, ModelTier::Local);
    }

    #[test]
    fn all_four_layers_resolve_in_documented_precedence() {
        let harness = PartialLinkedInPostPolicy {
            candidate_count: Some(4),
            ..Default::default()
        };
        let profile = PartialLinkedInPostPolicy {
            candidate_count: Some(5),
            ..Default::default()
        };
        let event = PartialLinkedInPostPolicy {
            candidate_count: Some(6),
            ..Default::default()
        };

        // builtin alone
        assert_eq!(
            resolve(LinkedInPostPolicy::default(), None, None, None).candidate_count,
            3
        );
        // harness beats builtin
        assert_eq!(
            resolve(LinkedInPostPolicy::default(), Some(&harness), None, None).candidate_count,
            4
        );
        // profile beats harness
        assert_eq!(
            resolve(
                LinkedInPostPolicy::default(),
                Some(&harness),
                Some(&profile),
                None
            )
            .candidate_count,
            5
        );
        // event beats profile
        assert_eq!(
            resolve(
                LinkedInPostPolicy::default(),
                Some(&harness),
                Some(&profile),
                Some(&event)
            )
            .candidate_count,
            6
        );
    }

    #[test]
    fn translate_enabled_overrides_independently_of_model_tiers() {
        let event = PartialLinkedInPostPolicy {
            translate_enabled: Some(false),
            ..Default::default()
        };
        let resolved = resolve(LinkedInPostPolicy::default(), None, None, Some(&event));
        assert!(!resolved.translate_enabled);
        assert_eq!(resolved.model_tiers.draft, ModelTier::Sonnet);
    }

    #[test]
    fn local_overrides_survive_on_all_three_stages() {
        let event = PartialLinkedInPostPolicy {
            model_tiers: Some(PartialModelTiers {
                draft: Some(ModelTier::Local),
                critic: Some(ModelTier::Local),
                translate: Some(ModelTier::Local),
            }),
            local: Some(PartialLocalConfig {
                endpoint: Some("http://localhost:8080".to_string()),
                model: Some("local-model".to_string()),
                constrained_json: Some(true),
            }),
            ..Default::default()
        };
        let resolved = resolve(LinkedInPostPolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.model_tiers.draft, ModelTier::Local);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Local);
        assert_eq!(resolved.local.endpoint, "http://localhost:8080");
        assert_eq!(resolved.local.model, "local-model");
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "model_tiers": { "draft": "local", "critic": "local", "translate": "local" },
            "max_critic_iterations": 2,
            "candidate_count": 4,
            "translate_enabled": false
        }"#;
        let partial: PartialLinkedInPostPolicy =
            serde_json::from_str(json).expect("valid PartialLinkedInPostPolicy JSON");
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().draft,
            Some(ModelTier::Local)
        );
        assert_eq!(partial.max_critic_iterations, Some(2));
        assert_eq!(partial.candidate_count, Some(4));
        assert_eq!(partial.translate_enabled, Some(false));
        // Not present in the JSON -> None on the local mirror.
        assert_eq!(partial.local, None);
    }

    #[test]
    fn validate_bounds_accepts_the_builtin_default() {
        assert!(validate_bounds(&LinkedInPostPolicy::default()).is_ok());
    }

    #[test]
    fn validate_bounds_rejects_zero_max_critic_iterations() {
        let policy = LinkedInPostPolicy {
            max_critic_iterations: 0,
            ..LinkedInPostPolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject zero");
        assert!(err.contains("max_critic_iterations"));
    }

    #[test]
    fn validate_bounds_rejects_max_critic_iterations_above_ceiling() {
        let policy = LinkedInPostPolicy {
            max_critic_iterations: MAX_CRITIC_ITERATIONS_CEILING + 1,
            ..LinkedInPostPolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject above ceiling");
        assert!(err.contains("max_critic_iterations"));
    }

    #[test]
    fn validate_bounds_accepts_max_critic_iterations_at_ceiling() {
        let policy = LinkedInPostPolicy {
            max_critic_iterations: MAX_CRITIC_ITERATIONS_CEILING,
            ..LinkedInPostPolicy::default()
        };
        assert!(validate_bounds(&policy).is_ok());
    }

    #[test]
    fn validate_bounds_rejects_zero_candidate_count() {
        let policy = LinkedInPostPolicy {
            candidate_count: 0,
            ..LinkedInPostPolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject zero");
        assert!(err.contains("candidate_count"));
    }

    #[test]
    fn validate_bounds_rejects_candidate_count_above_ceiling() {
        let policy = LinkedInPostPolicy {
            candidate_count: MAX_CANDIDATE_COUNT_CEILING + 1,
            ..LinkedInPostPolicy::default()
        };
        let err = validate_bounds(&policy).expect_err("should reject above ceiling");
        assert!(err.contains("candidate_count"));
    }

    #[test]
    fn validate_bounds_accepts_candidate_count_at_ceiling() {
        let policy = LinkedInPostPolicy {
            candidate_count: MAX_CANDIDATE_COUNT_CEILING,
            ..LinkedInPostPolicy::default()
        };
        assert!(validate_bounds(&policy).is_ok());
    }
}
