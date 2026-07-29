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

/// Materialization knob for the `EN.7.D` learning-artifact instance:
/// whether `MaterializeDocNode` writes a doc at all (`enabled`), where it
/// writes it (`corpus_root`), and whether the write is real vs a dry-run
/// (`write`). `enabled: false` is the documented switch that restores
/// pre-`EN.7.D` behavior exactly (the node stays in the graph and stamps a
/// skip result — see `nodes::materialize_doc::MaterializeDocNode::with_enabled`).
/// `corpus_root: None` means resolve via `crate::brain_root::resolve_brain_root`
/// (`ENGINE_BRAIN_ROOT`, then walk-up) rather than a policy-pinned root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaterializeConfig {
    pub enabled: bool,
    pub corpus_root: Option<String>,
    pub write: bool,
}

impl Default for MaterializeConfig {
    /// Built-in default: enabled, real writes, root resolved at run time.
    fn default() -> Self {
        Self {
            enabled: true,
            corpus_root: None,
            write: true,
        }
    }
}

/// Deserialize a nested `Option<Option<T>>` so an explicit JSON `null`
/// survives as `Some(None)` ("override to unset") rather than collapsing
/// into `None` ("not overridden"), which is what serde's stock `Option`
/// impl would do. Paired with `#[serde(default)]` on the struct, an
/// *absent* key still deserializes to `None`.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// All-optional mirror of [`MaterializeConfig`] for per-field overrides
/// across the four resolution layers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialMaterializeConfig {
    pub enabled: Option<bool>,
    /// `None` = not overridden; `Some(None)` = override to "resolve at run
    /// time"; `Some(Some(path))` = pin this root. An explicit JSON `null`
    /// deserializes to `Some(None)` — see [`double_option`].
    #[serde(deserialize_with = "double_option")]
    pub corpus_root: Option<Option<String>>,
    pub write: Option<bool>,
}

impl Overlay for MaterializeConfig {
    type Partial = PartialMaterializeConfig;

    /// Merge one override layer onto `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`). `corpus_root` is
    /// itself an `Option<String>`, so the partial wraps it in a second
    /// `Option` to distinguish "not overridden" (`None`) from "override to
    /// unset/resolve-at-runtime" (`Some(None)`).
    fn overlay(self, over: &Self::Partial) -> Self {
        MaterializeConfig {
            enabled: merge_opt(self.enabled, over.enabled),
            corpus_root: match &over.corpus_root {
                Some(v) => v.clone(),
                None => self.corpus_root,
            },
            write: merge_opt(self.write, over.write),
        }
    }
}

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
    /// `EN.7.D` learning-artifact materialization knob for the
    /// `MaterializeDocNode` instance wired between `DigestRenderNode` and
    /// `PersistToBrainNode`. Built-in default restores the write, at the
    /// runtime-resolved brain root, unconditionally.
    pub materialize: MaterializeConfig,
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
            materialize: MaterializeConfig::default(),
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
    pub materialize: Option<PartialMaterializeConfig>,
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
            materialize: match &over.materialize {
                Some(m) => base.materialize.overlay(m),
                None => base.materialize,
            },
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
        assert!(policy.materialize.enabled);
        assert_eq!(policy.materialize.corpus_root, None);
        assert!(policy.materialize.write);
    }

    #[test]
    fn materialize_config_default_matches_documented_builtin() {
        assert_eq!(
            MaterializeConfig::default(),
            MaterializeConfig {
                enabled: true,
                corpus_root: None,
                write: true,
            }
        );
    }

    #[test]
    fn materialize_enabled_overrides_independently_through_all_four_layers() {
        let harness = PartialContentPipelinePolicy {
            materialize: Some(PartialMaterializeConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), Some(&harness), None, None);
        assert!(!resolved.materialize.enabled);
        // Untouched fields still fall through to builtin.
        assert_eq!(resolved.materialize.corpus_root, None);
        assert!(resolved.materialize.write);

        let profile = PartialContentPipelinePolicy {
            materialize: Some(PartialMaterializeConfig {
                enabled: Some(true),
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
        assert!(resolved.materialize.enabled, "profile beats harness");

        let event = PartialContentPipelinePolicy {
            materialize: Some(PartialMaterializeConfig {
                enabled: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            ContentPipelinePolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert!(!resolved.materialize.enabled, "event beats profile");
    }

    #[test]
    fn materialize_corpus_root_overrides_independently_of_enabled_and_write() {
        let event = PartialContentPipelinePolicy {
            materialize: Some(PartialMaterializeConfig {
                corpus_root: Some(Some("/tmp/custom-corpus".to_string())),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, Some(&event));
        assert_eq!(
            resolved.materialize.corpus_root,
            Some("/tmp/custom-corpus".to_string())
        );
        // enabled/write untouched by the override, fall through to builtin.
        assert!(resolved.materialize.enabled);
        assert!(resolved.materialize.write);
    }

    #[test]
    fn materialize_write_overrides_independently_of_enabled_and_corpus_root() {
        let event = PartialContentPipelinePolicy {
            materialize: Some(PartialMaterializeConfig {
                write: Some(false),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(ContentPipelinePolicy::default(), None, None, Some(&event));
        assert!(!resolved.materialize.write);
        assert!(resolved.materialize.enabled);
        assert_eq!(resolved.materialize.corpus_root, None);
    }

    #[test]
    fn materialize_partial_overlay_setting_only_corpus_root_leaves_others_at_lower_layer() {
        let base = MaterializeConfig {
            enabled: false,
            corpus_root: None,
            write: false,
        };
        let over = PartialMaterializeConfig {
            corpus_root: Some(Some("/tmp/root".to_string())),
            ..Default::default()
        };
        let merged = base.clone().overlay(&over);
        assert_eq!(merged.corpus_root, Some("/tmp/root".to_string()));
        assert!(!merged.enabled, "untouched enabled falls through to base");
        assert!(!merged.write, "untouched write falls through to base");
    }

    #[test]
    fn materialize_serde_round_trip_holds_for_config_and_partial() {
        let config = MaterializeConfig {
            enabled: false,
            corpus_root: Some("/tmp/corpus".to_string()),
            write: false,
        };
        let json = serde_json::to_string(&config).expect("serialize MaterializeConfig");
        let round_tripped: MaterializeConfig =
            serde_json::from_str(&json).expect("deserialize MaterializeConfig");
        assert_eq!(round_tripped, config);

        let partial = PartialMaterializeConfig {
            enabled: Some(true),
            corpus_root: Some(Some("/tmp/corpus".to_string())),
            write: None,
        };
        let json = serde_json::to_string(&partial).expect("serialize PartialMaterializeConfig");
        let round_tripped: PartialMaterializeConfig =
            serde_json::from_str(&json).expect("deserialize PartialMaterializeConfig");
        assert_eq!(round_tripped, partial);
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
