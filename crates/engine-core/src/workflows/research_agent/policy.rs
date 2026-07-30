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

/// How hard a research node should look for reachable contact channels.
/// `off` restores the pre-`EN.4.E` behavior exactly (no contact directive at
/// all — the schema still carries the field, a run simply reports none).
/// `standard` visits the company's own contact-bearing surfaces
/// (contact/about/team page, footer, `mailto:`/`wa.me` links). `deep`
/// additionally sweeps public LinkedIn/Instagram/Facebook profiles and hunts
/// for the named decision-maker. Acquisition depth only ever changes the
/// *prompt* — the emitted JSON schema always describes `contacts`, so
/// `detect_kind` and the okf-core mapping are stable across every setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContactDepth {
    Off,
    Standard,
    Deep,
}

/// Per-stage contact-acquisition policy — mirrors [`ModelTiers`]'s per-stage
/// shape, plus `max_fetches`, the cap on the EXTRA page loads spent on
/// contact acquisition per run (contact acquisition costs real fetches and
/// real latency, so it is a policy knob per `CLAUDE.md` standing rule 6, not
/// a hardcoded prompt constant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContactEnrichment {
    pub research: ContactDepth,
    pub prospect: ContactDepth,
    pub max_fetches: u8,
}

impl Default for ContactEnrichment {
    /// Behavior-stable baseline: `standard` acquisition on both stages,
    /// capped at 4 extra fetches.
    fn default() -> Self {
        Self {
            research: ContactDepth::Standard,
            prospect: ContactDepth::Standard,
            max_fetches: 4,
        }
    }
}

/// Whether a finished `RESEARCH_AGENT` run dispatches an ingress-tail
/// `TriggerWorkflow` (`EN.6.E`) into `target_workflow_type`, and which
/// workflow type it targets. `enabled: false` is the behavior-stable
/// baseline — no dispatch happens today and this knob must not change
/// that until a profile or override turns it on. The node stays in the
/// declared graph at every setting and no-ops in place when disabled
/// (never a rewire); see `ingress_dispatch.rs`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngressDispatch {
    pub enabled: bool,
    pub target_workflow_type: String,
}

impl Default for IngressDispatch {
    /// Behavior-stable baseline: disabled, targeting `CONTENT_PIPELINE`.
    fn default() -> Self {
        Self {
            enabled: false,
            target_workflow_type: "CONTENT_PIPELINE".to_string(),
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
    /// Per-stage contact-acquisition depth + fetch cap (`EN.4.E`).
    pub contact_enrichment: ContactEnrichment,
    /// Terminal ingress-tail dispatch to `CONTENT_PIPELINE` (`EN.6.E`).
    pub ingress_dispatch: IngressDispatch,
}

impl Default for ResearchAgentPolicy {
    /// The safe default: normal verbosity, all-Sonnet tiers, prompt-cache
    /// off, standard contact enrichment on both stages, ingress dispatch
    /// disabled.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            model_tiers: ModelTiers::default(),
            local: LocalConfig::default(),
            contact_enrichment: ContactEnrichment::default(),
            ingress_dispatch: IngressDispatch::default(),
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
    pub contact_enrichment: Option<PartialContactEnrichment>,
    pub ingress_dispatch: Option<PartialIngressDispatch>,
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

/// All-optional mirror of [`ContactEnrichment`] for partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialContactEnrichment {
    pub research: Option<ContactDepth>,
    pub prospect: Option<ContactDepth>,
    pub max_fetches: Option<u8>,
}

fn merge_contact_enrichment(
    mut base: ContactEnrichment,
    over: &PartialContactEnrichment,
) -> ContactEnrichment {
    if let Some(v) = over.research {
        base.research = v;
    }
    if let Some(v) = over.prospect {
        base.prospect = v;
    }
    if let Some(v) = over.max_fetches {
        base.max_fetches = v;
    }
    base
}

/// All-optional mirror of [`IngressDispatch`] for partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialIngressDispatch {
    pub enabled: Option<bool>,
    pub target_workflow_type: Option<String>,
}

fn merge_ingress_dispatch(
    mut base: IngressDispatch,
    over: &PartialIngressDispatch,
) -> IngressDispatch {
    if let Some(v) = over.enabled {
        base.enabled = v;
    }
    if let Some(v) = &over.target_workflow_type {
        base.target_workflow_type = v.clone();
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
            contact_enrichment: match &over.contact_enrichment {
                Some(ce) => merge_contact_enrichment(base.contact_enrichment, ce),
                None => base.contact_enrichment,
            },
            ingress_dispatch: match &over.ingress_dispatch {
                Some(id) => merge_ingress_dispatch(base.ingress_dispatch, id),
                None => base.ingress_dispatch,
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

    #[test]
    fn contact_enrichment_builtin_default_is_standard_standard_4() {
        let policy = ResearchAgentPolicy::default();
        assert_eq!(policy.contact_enrichment.research, ContactDepth::Standard);
        assert_eq!(policy.contact_enrichment.prospect, ContactDepth::Standard);
        assert_eq!(policy.contact_enrichment.max_fetches, 4);
    }

    #[test]
    fn contact_depth_serializes_to_documented_snake_case_strings() {
        assert_eq!(
            serde_json::to_value(ContactDepth::Off).unwrap(),
            serde_json::json!("off")
        );
        assert_eq!(
            serde_json::to_value(ContactDepth::Standard).unwrap(),
            serde_json::json!("standard")
        );
        assert_eq!(
            serde_json::to_value(ContactDepth::Deep).unwrap(),
            serde_json::json!("deep")
        );
    }

    #[test]
    fn harness_default_overrides_builtin_for_contact_enrichment() {
        let harness = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                research: Some(ContactDepth::Deep),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(ResearchAgentPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.contact_enrichment.research, ContactDepth::Deep);
        // Untouched fields fall through to builtin.
        assert_eq!(resolved.contact_enrichment.prospect, ContactDepth::Standard);
        assert_eq!(resolved.contact_enrichment.max_fetches, 4);
    }

    #[test]
    fn profile_beats_harness_defaults_for_contact_enrichment() {
        let harness = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                max_fetches: Some(2),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                max_fetches: Some(8),
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
        assert_eq!(resolved.contact_enrichment.max_fetches, 8);
    }

    #[test]
    fn event_override_beats_profile_for_contact_enrichment() {
        let profile = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                prospect: Some(ContactDepth::Off),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                prospect: Some(ContactDepth::Deep),
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
        assert_eq!(resolved.contact_enrichment.prospect, ContactDepth::Deep);
    }

    #[test]
    fn partial_override_of_only_max_fetches_leaves_both_depths_untouched() {
        let event = PartialResearchAgentPolicy {
            contact_enrichment: Some(PartialContactEnrichment {
                max_fetches: Some(1),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(ResearchAgentPolicy::default(), None, None, Some(&event));
        assert_eq!(resolved.contact_enrichment.max_fetches, 1);
        assert_eq!(resolved.contact_enrichment.research, ContactDepth::Standard);
        assert_eq!(resolved.contact_enrichment.prospect, ContactDepth::Standard);
    }

    #[test]
    fn deserializes_partial_contact_enrichment_from_harness_json_shape() {
        let json = r#"{
            "contact_enrichment": { "research": "deep", "prospect": "off", "max_fetches": 8 }
        }"#;
        let partial: PartialResearchAgentPolicy =
            serde_json::from_str(json).expect("valid PartialResearchAgentPolicy JSON");
        let ce = partial.contact_enrichment.expect("contact_enrichment set");
        assert_eq!(ce.research, Some(ContactDepth::Deep));
        assert_eq!(ce.prospect, Some(ContactDepth::Off));
        assert_eq!(ce.max_fetches, Some(8));
    }

    #[test]
    fn ingress_dispatch_builtin_default_is_disabled_content_pipeline() {
        let policy = ResearchAgentPolicy::default();
        assert!(!policy.ingress_dispatch.enabled);
        assert_eq!(
            policy.ingress_dispatch.target_workflow_type,
            "CONTENT_PIPELINE"
        );
    }

    #[test]
    fn deserializes_partial_ingress_dispatch_from_harness_json_shape() {
        let json = r#"{
            "ingress_dispatch": { "enabled": true, "target_workflow_type": "CONTENT_PIPELINE" }
        }"#;
        let partial: PartialResearchAgentPolicy =
            serde_json::from_str(json).expect("valid PartialResearchAgentPolicy JSON");
        let id = partial.ingress_dispatch.expect("ingress_dispatch set");
        assert_eq!(id.enabled, Some(true));
        assert_eq!(
            id.target_workflow_type,
            Some("CONTENT_PIPELINE".to_string())
        );
    }

    #[test]
    fn partial_override_of_only_enabled_leaves_target_workflow_type_untouched() {
        let harness = PartialResearchAgentPolicy {
            ingress_dispatch: Some(PartialIngressDispatch {
                target_workflow_type: Some("CUSTOM_PIPELINE".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialResearchAgentPolicy {
            ingress_dispatch: Some(PartialIngressDispatch {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ResearchAgentPolicy::default(),
            Some(&harness),
            None,
            Some(&event),
        );
        assert!(resolved.ingress_dispatch.enabled);
        assert_eq!(
            resolved.ingress_dispatch.target_workflow_type,
            "CUSTOM_PIPELINE"
        );
    }

    #[test]
    fn ingress_dispatch_four_layer_precedence_holds() {
        let harness = PartialResearchAgentPolicy {
            ingress_dispatch: Some(PartialIngressDispatch {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        };
        let profile = PartialResearchAgentPolicy {
            ingress_dispatch: Some(PartialIngressDispatch {
                target_workflow_type: Some("PROFILE_PIPELINE".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialResearchAgentPolicy {
            ingress_dispatch: Some(PartialIngressDispatch {
                target_workflow_type: Some("EVENT_PIPELINE".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        // harness only.
        let resolved = resolve(ResearchAgentPolicy::default(), Some(&harness), None, None);
        assert!(resolved.ingress_dispatch.enabled);
        assert_eq!(
            resolved.ingress_dispatch.target_workflow_type,
            "CONTENT_PIPELINE"
        );

        // profile beats harness for target_workflow_type; enabled falls through from harness.
        let resolved = resolve(
            ResearchAgentPolicy::default(),
            Some(&harness),
            Some(&profile),
            None,
        );
        assert!(resolved.ingress_dispatch.enabled);
        assert_eq!(
            resolved.ingress_dispatch.target_workflow_type,
            "PROFILE_PIPELINE"
        );

        // event beats profile beats harness.
        let resolved = resolve(
            ResearchAgentPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert!(resolved.ingress_dispatch.enabled);
        assert_eq!(
            resolved.ingress_dispatch.target_workflow_type,
            "EVENT_PIPELINE"
        );
    }
}
