//! The resolved `SdlcPolicy` — runtime-configurable cost/time/quality levers
//! for the SDLC Flow (levers #1–#3 from
//! `planning/sdlc-token-time-economics/notes.md`), plus a `PartialPolicy`
//! mirror used by the two override layers and a `resolve` function that
//! merges all three layers into one concrete policy.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run
//! `SdlcFlowEvent` override, then a named `profile:` bundle, then
//! `planning/harness.json` `sdlc.policy` defaults, then built-in defaults.
//! The built-in [`SdlcPolicy::default`] MUST reproduce today's
//! (pre-EN.3.C) behavior exactly — turning any knob away from that
//! baseline is opt-in.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How verbose model-node prompts should ask the model to be (lever #2a).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputVerbosity {
    Terse,
    #[default]
    Normal,
    Verbose,
}

/// How the review gate is applied across a run's tasks (lever #3a).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    /// Today's behavior: every task routes to `ConsolidatedReviewNode`.
    #[default]
    PerTask,
    /// A trivial green task (small diff, first-pass green) skips per-task
    /// review; a non-trivial task still routes to review.
    TrivialSkip,
    /// Per-task review is collapsed into a single end-of-run review.
    EndOnly,
}

/// A model tier a stage can be resolved to (lever #3b). `Local` routes
/// through the `openai_compat_transport` (EN.3.C task 5); every other
/// variant maps to a concrete cloud model string in the consuming node.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    #[default]
    Sonnet,
    Haiku,
    Opus,
    Local,
}

/// Per-stage model tier assignment. Field names match the stage identities
/// used across `task_loop.rs`/`graph.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelTiers {
    pub implement: ModelTier,
    pub implement_simple: ModelTier,
    pub review: ModelTier,
    pub triage: ModelTier,
    pub generate: ModelTier,
}

impl Default for ModelTiers {
    /// All-Sonnet — the pre-EN.3.C baseline.
    fn default() -> Self {
        Self {
            implement: ModelTier::Sonnet,
            implement_simple: ModelTier::Sonnet,
            review: ModelTier::Sonnet,
            triage: ModelTier::Sonnet,
            generate: ModelTier::Sonnet,
        }
    }
}

/// Configuration for the `local` model tier's OpenAI-compatible transport
/// (EN.3.C task 5). Not present in the built-in default (no stage defaults
/// to `local`), but shaped here so `harness.json`/event overrides can supply
/// it once any stage opts into `local`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalConfig {
    /// Base URL of the OpenAI-compatible endpoint, e.g. `http://localhost:11434`.
    pub endpoint: String,
    /// Model name to request, e.g. `qwen2.5-coder:7b`.
    pub model: String,
    /// Whether to pass the stage's JSON schema as a constrained-decoding
    /// `response_format` and skip the JSON-repair retry for that stage.
    pub constrained_json: bool,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:11434".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            constrained_json: false,
        }
    }
}

/// Which pipeline stages `close-out` (EN.2.x) is allowed to reuse from a
/// prior flow record rather than re-running (lever #1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CloseOutReuse {
    pub validation: bool,
    pub review: bool,
    pub docs: bool,
}

/// The `close_out` policy block: just the reuse flags today, nested to
/// mirror the economics-notes JSON shape and leave room to grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CloseOut {
    pub reuse: CloseOutReuse,
}

/// The fully-resolved, per-run SDLC Flow policy — the merge of built-in
/// defaults, `harness.json`'s `sdlc.policy` defaults, and any per-run event
/// override, high->low precedence in that order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdlcPolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    pub review_mode: ReviewMode,
    pub review_skip_max_files: u32,
    pub review_skip_max_diff_lines: u32,
    pub model_tiers: ModelTiers,
    /// Configuration for the `local` model tier, when any stage uses it.
    pub local: LocalConfig,
    pub simple_task_max_files: u32,
    pub llm_triage: bool,
    pub max_attempts: u32,
    pub close_out: CloseOut,
}

impl Default for SdlcPolicy {
    /// The safe default: reproduces today's (pre-EN.3.C) behavior exactly.
    /// Normal verbosity, `per_task` review, all-Sonnet tiers, no close-out
    /// reuse, prompt-cache off, `llm_triage` false, `max_attempts` 3.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            review_mode: ReviewMode::PerTask,
            review_skip_max_files: 2,
            review_skip_max_diff_lines: 40,
            model_tiers: ModelTiers::default(),
            local: LocalConfig::default(),
            simple_task_max_files: 2,
            llm_triage: false,
            max_attempts: 3,
            close_out: CloseOut::default(),
        }
    }
}

/// All-optional mirror of [`SdlcPolicy`] used by the two override layers
/// (`harness.json`'s `sdlc.policy` and a per-run event's `policy` field).
/// Every field left `None` falls through to the next-lower-precedence
/// layer; `close_out` is deep-merged field-by-field via `PartialCloseOut`
/// rather than all-or-nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialPolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub review_mode: Option<ReviewMode>,
    pub review_skip_max_files: Option<u32>,
    pub review_skip_max_diff_lines: Option<u32>,
    pub model_tiers: Option<PartialModelTiers>,
    pub local: Option<PartialLocalConfig>,
    pub simple_task_max_files: Option<u32>,
    pub llm_triage: Option<bool>,
    pub max_attempts: Option<u32>,
    pub close_out: Option<PartialCloseOut>,
}

/// All-optional mirror of [`ModelTiers`] for per-stage partial overrides.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialModelTiers {
    pub implement: Option<ModelTier>,
    pub implement_simple: Option<ModelTier>,
    pub review: Option<ModelTier>,
    pub triage: Option<ModelTier>,
    pub generate: Option<ModelTier>,
}

/// All-optional mirror of [`LocalConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialLocalConfig {
    pub endpoint: Option<String>,
    pub model: Option<String>,
    pub constrained_json: Option<bool>,
}

/// All-optional mirror of [`CloseOutReuse`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialCloseOutReuse {
    pub validation: Option<bool>,
    pub review: Option<bool>,
    pub docs: Option<bool>,
}

/// All-optional mirror of [`CloseOut`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialCloseOut {
    pub reuse: Option<PartialCloseOutReuse>,
}

fn merge_opt<T>(lower: T, higher: Option<T>) -> T {
    higher.unwrap_or(lower)
}

fn merge_model_tiers(mut base: ModelTiers, over: &PartialModelTiers) -> ModelTiers {
    if let Some(v) = over.implement {
        base.implement = v;
    }
    if let Some(v) = over.implement_simple {
        base.implement_simple = v;
    }
    if let Some(v) = over.review {
        base.review = v;
    }
    if let Some(v) = over.triage {
        base.triage = v;
    }
    if let Some(v) = over.generate {
        base.generate = v;
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

fn merge_close_out_reuse(mut base: CloseOutReuse, over: &PartialCloseOutReuse) -> CloseOutReuse {
    if let Some(v) = over.validation {
        base.validation = v;
    }
    if let Some(v) = over.review {
        base.review = v;
    }
    if let Some(v) = over.docs {
        base.docs = v;
    }
    base
}

fn merge_close_out(mut base: CloseOut, over: &PartialCloseOut) -> CloseOut {
    if let Some(reuse) = &over.reuse {
        base.reuse = merge_close_out_reuse(base.reuse, reuse);
    }
    base
}

/// Apply one override layer on top of a `base` policy, field-by-field
/// (`Some` in `over` wins, `None` falls through to `base`).
fn apply_override(base: SdlcPolicy, over: &PartialPolicy) -> SdlcPolicy {
    SdlcPolicy {
        output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
        prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
        review_mode: merge_opt(base.review_mode, over.review_mode),
        review_skip_max_files: merge_opt(base.review_skip_max_files, over.review_skip_max_files),
        review_skip_max_diff_lines: merge_opt(
            base.review_skip_max_diff_lines,
            over.review_skip_max_diff_lines,
        ),
        model_tiers: match &over.model_tiers {
            Some(mt) => merge_model_tiers(base.model_tiers, mt),
            None => base.model_tiers,
        },
        local: match &over.local {
            Some(l) => merge_local(base.local, l),
            None => base.local,
        },
        simple_task_max_files: merge_opt(base.simple_task_max_files, over.simple_task_max_files),
        llm_triage: merge_opt(base.llm_triage, over.llm_triage),
        max_attempts: merge_opt(base.max_attempts, over.max_attempts),
        close_out: match &over.close_out {
            Some(co) => merge_close_out(base.close_out, co),
            None => base.close_out,
        },
    }
}

/// Resolve the four policy layers into one concrete [`SdlcPolicy`],
/// high->low precedence: `event_override` > `profile` > `harness_defaults`
/// > `builtin`.
///
/// `builtin` is normally `SdlcPolicy::default()`, taken as a parameter so
/// callers/tests can exercise the merge against any base. `profile` is a
/// named bundle (see `profiles::profile_by_name` and `harness.json`'s
/// `sdlc.profiles`) resolved by the caller before this function runs.
#[must_use]
pub fn resolve(
    builtin: SdlcPolicy,
    harness_defaults: Option<&PartialPolicy>,
    profile: Option<&PartialPolicy>,
    event_override: Option<&PartialPolicy>,
) -> SdlcPolicy {
    let mut resolved = builtin;
    if let Some(harness) = harness_defaults {
        resolved = apply_override(resolved, harness);
    }
    if let Some(profile) = profile {
        resolved = apply_override(resolved, profile);
    }
    if let Some(event) = event_override {
        resolved = apply_override(resolved, event);
    }
    resolved
}

/// Convenience: a `BTreeMap` view of the resolved per-stage tiers, keyed by
/// stage identity, for callers (e.g. telemetry) that want to iterate
/// stage->tier without matching on `ModelTiers`' fixed fields.
#[must_use]
pub fn model_tiers_by_stage(tiers: &ModelTiers) -> BTreeMap<&'static str, ModelTier> {
    let mut map = BTreeMap::new();
    map.insert("implement", tiers.implement);
    map.insert("implement_simple", tiers.implement_simple);
    map.insert("review", tiers.review);
    map.insert("triage", tiers.triage);
    map.insert("generate", tiers.generate);
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::sdlc_flow::profiles;

    #[test]
    fn builtin_default_matches_pre_en_3_c_baseline() {
        let policy = SdlcPolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert_eq!(policy.review_mode, ReviewMode::PerTask);
        assert_eq!(policy.model_tiers.implement, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.implement_simple, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.review, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.triage, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.generate, ModelTier::Sonnet);
        assert!(!policy.close_out.reuse.validation);
        assert!(!policy.close_out.reuse.review);
        assert!(!policy.close_out.reuse.docs);
        assert!(!policy.prompt_cache);
        assert!(!policy.llm_triage);
        assert_eq!(policy.max_attempts, 3);
    }

    #[test]
    fn resolve_with_no_overrides_returns_builtin() {
        let resolved = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(resolved, SdlcPolicy::default());
    }

    #[test]
    fn harness_default_overrides_builtin_for_output_verbosity() {
        let harness = PartialPolicy {
            output_verbosity: Some(OutputVerbosity::Terse),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.review_mode, ReviewMode::PerTask);
    }

    #[test]
    fn event_override_beats_harness_default_for_review_mode() {
        let harness = PartialPolicy {
            review_mode: Some(ReviewMode::TrivialSkip),
            ..Default::default()
        };
        let event = PartialPolicy {
            review_mode: Some(ReviewMode::EndOnly),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.review_mode, ReviewMode::EndOnly);
    }

    #[test]
    fn event_override_beats_harness_default_for_model_tiers_stage() {
        let harness = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                implement: Some(ModelTier::Haiku),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            model_tiers: Some(PartialModelTiers {
                implement: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.model_tiers.implement, ModelTier::Opus);
        // Other stages untouched by either override fall through to builtin.
        assert_eq!(resolved.model_tiers.review, ModelTier::Sonnet);
    }

    #[test]
    fn harness_default_overrides_builtin_for_max_attempts() {
        let harness = PartialPolicy {
            max_attempts: Some(5),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.max_attempts, 5);
    }

    #[test]
    fn event_override_beats_harness_default_for_close_out_reuse() {
        let harness = PartialPolicy {
            close_out: Some(PartialCloseOut {
                reuse: Some(PartialCloseOutReuse {
                    validation: Some(true),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            close_out: Some(PartialCloseOut {
                reuse: Some(PartialCloseOutReuse {
                    review: Some(true),
                    ..Default::default()
                }),
            }),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        // Deep-merge: harness's `validation: true` survives the event layer,
        // event's `review: true` layers on top, `docs` still falls to builtin.
        assert!(resolved.close_out.reuse.validation);
        assert!(resolved.close_out.reuse.review);
        assert!(!resolved.close_out.reuse.docs);
    }

    #[test]
    fn deserializes_partial_policy_from_harness_json_shape() {
        let json = r#"{
            "output_verbosity": "terse",
            "review_mode": "trivial_skip",
            "review_skip_max_files": 2,
            "review_skip_max_diff_lines": 40,
            "model_tiers": { "implement": "haiku", "generate": "opus" },
            "llm_triage": false
        }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        assert_eq!(partial.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(partial.review_mode, Some(ReviewMode::TrivialSkip));
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().implement,
            Some(ModelTier::Haiku)
        );
        assert_eq!(
            partial.model_tiers.as_ref().unwrap().generate,
            Some(ModelTier::Opus)
        );
        // Not present in the JSON -> None, falls through on merge.
        assert_eq!(partial.model_tiers.as_ref().unwrap().review, None);
    }

    #[test]
    fn resolve_with_only_profile_matches_documented_cheap_fast_policy() {
        let cheap_fast = profiles::cheap_fast();
        let resolved = resolve(SdlcPolicy::default(), None, Some(&cheap_fast), None);
        assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
        // Untouched knobs still fall through to builtin.
        assert_eq!(resolved.model_tiers.generate, ModelTier::Sonnet);
        assert!(!resolved.prompt_cache);
    }

    #[test]
    fn event_override_beats_profile_for_one_field_while_profile_supplies_the_rest() {
        let cheap_fast = profiles::cheap_fast();
        let event = PartialPolicy {
            max_attempts: Some(9),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), None, Some(&cheap_fast), Some(&event));
        // Event-inline field wins.
        assert_eq!(resolved.max_attempts, 9);
        // Profile still supplies everything the event didn't touch.
        assert_eq!(resolved.model_tiers.implement, ModelTier::Haiku);
        assert_eq!(resolved.model_tiers.triage, ModelTier::Local);
        assert_eq!(resolved.model_tiers.review, ModelTier::Local);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    }

    #[test]
    fn profile_beats_harness_defaults_for_overlapping_fields() {
        let harness = PartialPolicy {
            review_mode: Some(ReviewMode::EndOnly),
            ..Default::default()
        };
        let cheap_fast = profiles::cheap_fast();
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&cheap_fast),
            None,
        );
        // profile's review_mode wins over harness_defaults'.
        assert_eq!(resolved.review_mode, ReviewMode::TrivialSkip);
    }
}
