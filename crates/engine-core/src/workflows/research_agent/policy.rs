//! `ResearchAgentPolicy` / `PartialResearchAgentPolicy` and the `Policy`
//! trait delegation to `crate::policy::resolve::resolve` — filled in task 2.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `research_agent.policy` defaults, then built-in
//! defaults. The built-in [`ResearchAgentPolicy::default`] is a documented,
//! behavior-stable baseline (all-Sonnet, normal verbosity, prompt cache
//! off) for both stages.
//!
//! Both stages (`research`, `prospect`) are cloud-only — they wrap
//! `ClaudeCodeStep` with `WebSearch`/`WebFetch` tools granted, which a
//! local single-shot endpoint cannot serve. `LocalConfig` is still carried
//! for API-shape parity with `crate::policy::tier`, but no stage default
//! ever resolves to `ModelTier::Local`.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

/// Per-stage cloud model tier assignment for the two terminal nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub research: ModelTier,
    pub prospect: ModelTier,
}

impl Default for ModelTiers {
    /// All-Sonnet — the behavior-stable baseline.
    fn default() -> Self {
        Self {
            research: ModelTier::Sonnet,
            prospect: ModelTier::Sonnet,
        }
    }
}

/// The fully-resolved, per-run Research Agent policy — the merge of
/// built-in defaults, `harness.json`'s `research_agent.policy` defaults, a
/// named `profile`, and any per-run event override, high->low precedence in
/// that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResearchAgentPolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub model_tiers: ModelTiers,
    /// Configuration for the `local` model tier — carried for API-shape
    /// parity only; neither `research` nor `prospect` ever resolves to it.
    pub local: LocalConfig,
}

impl Default for ResearchAgentPolicy {
    /// The safe default: normal verbosity, all-Sonnet tiers, prompt-cache
    /// off.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            model_tiers: ModelTiers::default(),
            local: LocalConfig::default(),
        }
    }
}

/// All-optional mirror of [`ModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub research: Option<ModelTier>,
    pub prospect: Option<ModelTier>,
}

/// All-optional mirror of [`ResearchAgentPolicy`] used by the override
/// layers (`harness.json`'s `research_agent.policy`, a named `profile`, and
/// a per-run event's `policy` field). Every field left `None` falls through
/// to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialResearchAgentPolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.research {
        base.research = v;
    }
    if let Some(v) = over.prospect {
        base.prospect = v;
    }
    base
}

impl crate::policy::Policy for ResearchAgentPolicy {
    type Partial = PartialResearchAgentPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialResearchAgentPolicy) -> Self {
        let base = self;
        ResearchAgentPolicy {
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
        }
    }
}

/// Resolve the four policy layers into one concrete [`ResearchAgentPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats `builtin`. Delegates to the generic
/// `crate::policy::resolve::resolve` now that `ResearchAgentPolicy`
/// implements `crate::policy::Policy`.
///
/// `builtin` is normally `ResearchAgentPolicy::default()`, taken as a
/// parameter so callers/tests can exercise the merge against any base.
/// `profile` is a named bundle (see `profiles::profile_by_name` and
/// `harness.json`'s `research_agent.profiles`) resolved by the caller
/// before this function runs.
#[must_use]
pub fn resolve(
    builtin: ResearchAgentPolicy,
    harness_defaults: Option<&PartialResearchAgentPolicy>,
    profile: Option<&PartialResearchAgentPolicy>,
    event_override: Option<&PartialResearchAgentPolicy>,
) -> ResearchAgentPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = ResearchAgentPolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.model_tiers.research, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.prospect, ModelTier::Sonnet);
        assert!(!policy.prompt_cache);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(ResearchAgentPolicy::default(), None, None, None);
        assert_eq!(resolved, ResearchAgentPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialResearchAgentPolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(ResearchAgentPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.research, ModelTier::Sonnet);
    }

    #[test]
    fn profile_beats_harness_defaults_for_model_tiers_stage() {
        let harness = PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                research: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                research: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ResearchAgentPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.model_tiers.research, ModelTier::Opus);
        // Other stage untouched by either override falls through to builtin.
        assert_eq!(resolved.model_tiers.prospect, ModelTier::Sonnet);
    }

    #[test]
    fn event_override_beats_profile_for_model_tiers_stage() {
        let profile = PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                prospect: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                prospect: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ResearchAgentPolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.prospect, ModelTier::Opus);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "prompt_cache": true,
            "model_tiers": { "research": "haiku", "prospect": "opus" }
        }"#;
        let partial: PartialResearchAgentPolicy =
            serde_json::from_str(json).expect("valid PartialResearchAgentPolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.prompt_cache, Some(true));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().research,
            Some(ModelTier::Haiku)
        );
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().prospect,
            Some(ModelTier::Opus)
        );
        // Not present in the JSON -> None on the local mirror.
        assert_eq!(partial.local, None);
    }

    #[test]
    fn no_stage_ever_defaults_to_local_tier() {
        let policy = ResearchAgentPolicy::default();
        assert_ne!(policy.model_tiers.research, ModelTier::Local);
        assert_ne!(policy.model_tiers.prospect, ModelTier::Local);
    }
}
