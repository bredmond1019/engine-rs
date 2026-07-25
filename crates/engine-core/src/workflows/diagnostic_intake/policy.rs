//! `DiagnosticIntakePolicy` / `PartialDiagnosticIntakePolicy` and the
//! `Policy` trait delegation to `crate::policy::resolve::resolve`.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run event
//! `policy` override, then a named `profile:` bundle, then
//! `planning/harness.json` `diagnostic_intake.policy` defaults, then
//! built-in defaults. The built-in [`DiagnosticIntakePolicy::default`] is a
//! documented, behavior-stable baseline (Sonnet tier, normal verbosity,
//! prompt cache off) for its sole `extract` stage.
//!
//! Unlike `research_agent`'s two cloud-only stages, this workflow's single
//! `extract` stage is **Local-eligible** — the pure-extraction task suits a
//! local coder model, so `ModelTier::Local` is a valid resolved value for
//! `extract` and `local: LocalConfig` carries the endpoint +
//! `constrained_json` config the Local-tier rewire in `graph.rs` consumes.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};

/// Per-stage model tier assignment. `DIAGNOSTIC_INTAKE` has exactly one
/// stage: the terminal `IntakeExtractNode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub extract: ModelTier,
}

impl Default for ModelTiers {
    /// Sonnet — the behavior-stable baseline. `extract` may still be
    /// resolved to `ModelTier::Local` via an override layer.
    fn default() -> Self {
        Self {
            extract: ModelTier::Sonnet,
        }
    }
}

/// The fully-resolved, per-run Diagnostic Intake policy — the merge of
/// built-in defaults, `harness.json`'s `diagnostic_intake.policy` defaults,
/// a named `profile`, and any per-run event override, high->low precedence
/// in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiagnosticIntakePolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub model_tiers: ModelTiers,
    /// Configuration for the `local` model tier — consumed by
    /// `graph::registry_for_policy` when `model_tiers.extract ==
    /// ModelTier::Local`.
    pub local: LocalConfig,
}

impl Default for DiagnosticIntakePolicy {
    /// The safe default: normal verbosity, Sonnet extract tier, prompt-cache
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

/// All-optional mirror of [`ModelTiers`] for the `extract` stage override.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub extract: Option<ModelTier>,
}

/// All-optional mirror of [`LocalConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialLocalConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub constrained_json: Option<bool>,
}

/// All-optional mirror of [`DiagnosticIntakePolicy`] used by the override
/// layers (`harness.json`'s `diagnostic_intake.policy`, a named `profile`,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialDiagnosticIntakePolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
}

fn merge_opt<T>(lower: T, higher: Option<T>) -> T {
    higher.unwrap_or(lower)
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.extract {
        base.extract = v;
    }
    base
}

fn merge_local(mut base: LocalConfig, over: &PartialLocalConfig) -> LocalConfig {
    if let Some(v) = &over.endpoint {
        base.endpoint = v.clone();
    }
    if let Some(v) = &over.model {
        base.model = v.clone();
    }
    if let Some(v) = over.constrained_json {
        base.constrained_json = v;
    }
    base
}

impl crate::policy::Policy for DiagnosticIntakePolicy {
    type Partial = PartialDiagnosticIntakePolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialDiagnosticIntakePolicy) -> Self {
        apply_override(self, over)
    }
}

/// Apply one override layer on top of a `base` policy, field-by-field
/// (`Some` in `over` wins, `None` falls through to `base`).
fn apply_override(
    base: DiagnosticIntakePolicy,
    over: &PartialDiagnosticIntakePolicy,
) -> DiagnosticIntakePolicy {
    DiagnosticIntakePolicy {
        output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
        prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
        model_tiers: match &over.model_tiers {
            Some(mt) => merge_model_tiers(base.model_tiers, mt),
            None => base.model_tiers,
        },
        local: match &over.local {
            Some(l) => merge_local(base.local, l),
            None => base.local,
        },
    }
}

/// Resolve the four policy layers into one concrete
/// [`DiagnosticIntakePolicy`], high->low precedence: `event_override` beats
/// `profile` beats `harness_defaults` beats `builtin`. Delegates to the
/// generic `crate::policy::resolve::resolve` now that
/// `DiagnosticIntakePolicy` implements `crate::policy::Policy`.
///
/// `builtin` is normally `DiagnosticIntakePolicy::default()`, taken as a
/// parameter so callers/tests can exercise the merge against any base.
/// `profile` is a named bundle (see `profiles::profile_by_name` and
/// `harness.json`'s `diagnostic_intake.profiles`) resolved by the caller
/// before this function runs.
#[must_use]
pub fn resolve(
    builtin: DiagnosticIntakePolicy,
    harness_defaults: Option<&PartialDiagnosticIntakePolicy>,
    profile: Option<&PartialDiagnosticIntakePolicy>,
    event_override: Option<&PartialDiagnosticIntakePolicy>,
) -> DiagnosticIntakePolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_default_is_behavior_stable_baseline() {
        let policy = DiagnosticIntakePolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.model_tiers.extract, ModelTier::Sonnet);
        assert!(!policy.prompt_cache);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(DiagnosticIntakePolicy::default(), None, None, None);
        assert_eq!(resolved, DiagnosticIntakePolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialDiagnosticIntakePolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(
            DiagnosticIntakePolicy::default(),
            Some(&harness),
            None,
            None,
        );
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.extract, ModelTier::Sonnet);
    }

    #[test]
    fn profile_beats_harness_defaults_for_extract_tier() {
        let harness = PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Haiku),
            }),
            ..Default::default()
        };
        let profile = PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Opus),
            }),
            ..Default::default()
        };
        let resolved = resolve(
            DiagnosticIntakePolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert_eq!(resolved.model_tiers.extract, ModelTier::Opus);
    }

    #[test]
    fn event_override_beats_profile_for_extract_tier() {
        let profile = PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Haiku),
            }),
            ..Default::default()
        };
        let event = PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Opus),
            }),
            ..Default::default()
        };
        let resolved = resolve(
            DiagnosticIntakePolicy::default(),
            None,
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.extract, ModelTier::Opus);
    }

    #[test]
    fn local_tier_override_survives_resolution_with_local_config() {
        let event = PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Local),
            }),
            local: Some(PartialLocalConfig {
                endpoint: Some("http://localhost:11434".to_string()),
                model: Some("qwen2.5-coder:7b".to_string()),
                constrained_json: Some(true),
            }),
            ..Default::default()
        };
        let resolved = resolve(DiagnosticIntakePolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.model_tiers.extract, ModelTier::Local);
        assert_eq!(resolved.local.endpoint, "http://localhost:11434");
        assert_eq!(resolved.local.model, "qwen2.5-coder:7b");
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "prompt_cache": true,
            "model_tiers": { "extract": "local" },
            "local": { "constrained_json": true }
        }"#;
        let partial: PartialDiagnosticIntakePolicy =
            serde_json::from_str(json).expect("valid PartialDiagnosticIntakePolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.prompt_cache, Some(true));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().extract,
            Some(ModelTier::Local)
        );
        assert_eq!(partial.local.as_ref().unwrap().constrained_json, Some(true));
        // Not present in the JSON -> None on the local mirror's other fields.
        assert_eq!(partial.local.as_ref().unwrap().endpoint, None);
    }
}
