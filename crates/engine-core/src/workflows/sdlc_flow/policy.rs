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
//!
//! **EN.4.0:** `SdlcPolicy` now implements the generic `crate::policy::Policy`
//! trait, and this module's `resolve` delegates to the generic
//! `crate::policy::resolve::resolve`. `OutputVerbosity`/`ModelTier`/
//! `LocalConfig` are re-exported from `crate::policy::tier` (byte-identical
//! serde reprs to the pre-hoist types) rather than redefined here.
//!
//! **EN.5.D task 2:** the hand-written `merge_opt`/`merge_local`/
//! `apply_override` trio is gone — `local: LocalConfig` merges via the
//! shared `crate::policy::Overlay` surface (`policy::overlay`), and the
//! top-level field merge is inlined directly into `Policy::apply` rather
//! than a private free function.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub use crate::policy::tier::{LocalConfig, ModelTier, OutputVerbosity};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};

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

/// How much of a task's own check suite `TestTaskNode` runs at a given
/// per-task tripwire (standing rule 6's cost/latency knob for EN.3.D).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestDepth {
    /// Today's Rust behavior (the behavior-stable built-in default): run
    /// every selected check's `command` verbatim.
    #[default]
    Full,
    /// Substitute a check's `fastCommand` for its `command` when the check
    /// declares one, falling back to `command` when it does not. A task
    /// carrying its own `validation_commands` ignores this knob entirely
    /// (see `select_task_checks`'s precedence rule in `task_loop.rs`).
    Fast,
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

/// Per-stage whole-call timeout, in **seconds**, for the model stages that
/// run through `task_loop.rs`'s `apply_policy`. Field names match the three
/// `Stage` variants (`Implement`/`Triage`/`Review`).
///
/// `None` — the behavior-stable built-in default for every field — means
/// "set nothing", leaving `claude_code_rs::Config::timeout` at its own
/// `None`, which is that crate's unconfigured 300s default. Setting a field
/// widens (or narrows) only that stage's `execute()` timeout.
///
/// Deliberately **not** covering `GenerateTasksNode`/`PatchDocsNode`: those
/// two nodes construct their `Config` by hand and have no `apply_policy`
/// path at all today (see this ticket's Description) — giving them one is a
/// separate piece of work.
///
/// Unlike [`ModelTiers`], this derives `Default`: all-`None` is exactly what
/// `#[derive(Default)]` produces, so hand-writing it would only add a place
/// to get it wrong.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub review: Option<u64>,
}

/// Whether (and how much of) the previous attempt's failure output is fed
/// back into `ImplementTaskNode`'s prompt on a retry.
///
/// **Deliberate deviation from CLAUDE.md standing rule 6's
/// "behavior-stable built-in default".** Every other knob in this file
/// defaults to reproducing today's behavior; this one does not. Feeding the
/// prior failure back into the retry prompt *is* the fix this knob ships —
/// a default-off knob would leave the retry loop structurally incapable of
/// self-correction (the live bail that motivated it: three byte-identical
/// attempts against two trivial compile errors). What the knob buys is a
/// cost bound (`max_chars`) and an escape hatch, not opt-in-ness.
///
/// Hand-written `Default` (like [`ModelTiers`], unlike [`CallTimeouts`])
/// because the intended default is NOT the all-zero/all-false derive value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryFeedback {
    /// Feed the prior attempt's failure output into the retry prompt.
    pub enabled: bool,
    /// Upper bound, in characters, on the whole rendered feedback block —
    /// so repeated retries cannot grow the prompt without limit.
    pub max_chars: u32,
}

impl Default for RetryFeedback {
    fn default() -> Self {
        Self {
            enabled: true,
            max_chars: 4000,
        }
    }
}

/// All-optional mirror of [`RetryFeedback`]. Mirrors [`PartialModelTiers`]'
/// derive set exactly — notably **no** `Copy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialRetryFeedback {
    pub enabled: Option<bool>,
    pub max_chars: Option<u32>,
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
    /// How much of a task's own check suite `TestTaskNode` runs (EN.3.D).
    pub test_depth: TestDepth,
    pub model_tiers: ModelTiers,
    /// Per-stage whole-call timeout in seconds; all-`None` (no override) by
    /// default, i.e. `claude-code-rs`'s own 300s default applies.
    pub timeouts: CallTimeouts,
    /// Configuration for the `local` model tier, when any stage uses it.
    pub local: LocalConfig,
    pub simple_task_max_files: u32,
    pub llm_triage: bool,
    pub max_attempts: u32,
    pub close_out: CloseOut,
    /// Whether the previous attempt's failure output is fed back into
    /// `ImplementTaskNode`'s retry prompt, and how large that block may get.
    pub retry_feedback: RetryFeedback,
}

impl Default for SdlcPolicy {
    /// The safe default: reproduces today's (pre-EN.3.C) behavior exactly.
    /// Normal verbosity, `per_task` review, all-Sonnet tiers, no close-out
    /// reuse, prompt-cache off, `llm_triage` false, `max_attempts` 3,
    /// `test_depth: full`, and no per-stage call-timeout overrides (all
    /// `None` — `claude-code-rs`'s own 300s default applies).
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            review_mode: ReviewMode::PerTask,
            review_skip_max_files: 2,
            review_skip_max_diff_lines: 40,
            test_depth: TestDepth::Full,
            model_tiers: ModelTiers::default(),
            timeouts: CallTimeouts::default(),
            local: LocalConfig::default(),
            simple_task_max_files: 2,
            llm_triage: false,
            max_attempts: 3,
            close_out: CloseOut::default(),
            // NOT behavior-stable, deliberately — see `RetryFeedback`'s docs.
            retry_feedback: RetryFeedback::default(),
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
    pub test_depth: Option<TestDepth>,
    pub model_tiers: Option<PartialModelTiers>,
    pub timeouts: Option<PartialCallTimeouts>,
    pub local: Option<PartialLocalConfig>,
    pub simple_task_max_files: Option<u32>,
    pub llm_triage: Option<bool>,
    pub max_attempts: Option<u32>,
    pub close_out: Option<PartialCloseOut>,
    pub retry_feedback: Option<PartialRetryFeedback>,
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

/// All-optional mirror of [`CallTimeouts`] for per-stage partial overrides.
/// Mirrors [`PartialModelTiers`]' derive set exactly — notably **no** `Copy`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialCallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub review: Option<u64>,
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

/// Merge one `PartialCallTimeouts` override layer over a `CallTimeouts`
/// base, field by field. A `None` field in `over` leaves the base value
/// (itself possibly `None`) untouched — so an absent override never turns a
/// timeout *off*, it just declines to set one.
fn merge_call_timeouts(mut base: CallTimeouts, over: &PartialCallTimeouts) -> CallTimeouts {
    if let Some(v) = over.implement {
        base.implement = Some(v);
    }
    if let Some(v) = over.triage {
        base.triage = Some(v);
    }
    if let Some(v) = over.review {
        base.review = Some(v);
    }
    base
}

/// Merge one `PartialRetryFeedback` override layer over a `RetryFeedback`
/// base, field by field. Mirrors [`merge_model_tiers`].
fn merge_retry_feedback(mut base: RetryFeedback, over: &PartialRetryFeedback) -> RetryFeedback {
    if let Some(v) = over.enabled {
        base.enabled = v;
    }
    if let Some(v) = over.max_chars {
        base.max_chars = v;
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

impl crate::policy::Policy for SdlcPolicy {
    type Partial = PartialPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`).
    fn apply(self, over: &PartialPolicy) -> Self {
        let base = self;
        SdlcPolicy {
            output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
            prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
            review_mode: merge_opt(base.review_mode, over.review_mode),
            review_skip_max_files: merge_opt(
                base.review_skip_max_files,
                over.review_skip_max_files,
            ),
            review_skip_max_diff_lines: merge_opt(
                base.review_skip_max_diff_lines,
                over.review_skip_max_diff_lines,
            ),
            test_depth: merge_opt(base.test_depth, over.test_depth),
            model_tiers: match &over.model_tiers {
                Some(mt) => merge_model_tiers(base.model_tiers, mt),
                None => base.model_tiers,
            },
            timeouts: match &over.timeouts {
                Some(t) => merge_call_timeouts(base.timeouts, t),
                None => base.timeouts,
            },
            local: match &over.local {
                Some(l) => base.local.overlay(l),
                None => base.local,
            },
            simple_task_max_files: merge_opt(
                base.simple_task_max_files,
                over.simple_task_max_files,
            ),
            llm_triage: merge_opt(base.llm_triage, over.llm_triage),
            max_attempts: merge_opt(base.max_attempts, over.max_attempts),
            close_out: match &over.close_out {
                Some(co) => merge_close_out(base.close_out, co),
                None => base.close_out,
            },
            retry_feedback: match &over.retry_feedback {
                Some(rf) => merge_retry_feedback(base.retry_feedback, rf),
                None => base.retry_feedback,
            },
        }
    }
}

/// Resolve the four policy layers into one concrete [`SdlcPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats `builtin`. Delegates to the generic
/// `crate::policy::resolve::resolve` now that `SdlcPolicy` implements
/// `crate::policy::Policy` (EN.4.0).
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
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
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
    fn builtin_default_test_depth_is_full() {
        assert_eq!(SdlcPolicy::default().test_depth, TestDepth::Full);
    }

    /// Change-detector: the whole point of this knob is that adding it did
    /// not change what an existing run does. All-`None` means every stage's
    /// `Config.timeout` stays `None`, i.e. `claude-code-rs`'s 300s default.
    #[test]
    fn builtin_default_timeouts_are_none() {
        let timeouts = SdlcPolicy::default().timeouts;
        assert_eq!(timeouts.implement, None);
        assert_eq!(timeouts.triage, None);
        assert_eq!(timeouts.review, None);
        assert_eq!(timeouts, CallTimeouts::default());
    }

    /// Change-detector pinning this knob's built-in default. Unlike the
    /// other change-detectors in this module it does NOT pin "today's
    /// behavior" — it pins a deliberate behavior change: `enabled: true` is
    /// the fix (see `RetryFeedback`'s docs for the rule-6 deviation).
    #[test]
    fn builtin_default_retry_feedback_is_enabled() {
        let rf = SdlcPolicy::default().retry_feedback;
        assert!(rf.enabled);
        assert_eq!(rf.max_chars, 4000);
        assert_eq!(
            rf,
            RetryFeedback {
                enabled: true,
                max_chars: 4000
            }
        );
    }

    #[test]
    fn merge_retry_feedback_overrides_only_the_fields_it_sets() {
        let base = RetryFeedback {
            enabled: true,
            max_chars: 4000,
        };
        let over = PartialRetryFeedback {
            enabled: None,
            max_chars: Some(500),
        };
        let merged = merge_retry_feedback(base, &over);
        // `None` in the override leaves the base value alone.
        assert!(merged.enabled);
        assert_eq!(merged.max_chars, 500);

        let off = PartialRetryFeedback {
            enabled: Some(false),
            max_chars: None,
        };
        let merged = merge_retry_feedback(base, &off);
        assert!(!merged.enabled);
        assert_eq!(merged.max_chars, 4000);
    }

    #[test]
    fn retry_feedback_resolves_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                enabled: Some(false),
                max_chars: Some(100),
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                enabled: Some(true),
                max_chars: Some(200),
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            retry_feedback: Some(PartialRetryFeedback {
                max_chars: Some(300),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin.
        assert_eq!(resolved.retry_feedback.max_chars, 300);
        // event left `enabled` unset, so the profile's value survives.
        assert!(resolved.retry_feedback.enabled);

        // Only harness -> harness wins over builtin.
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert!(!resolved.retry_feedback.enabled);
        assert_eq!(resolved.retry_feedback.max_chars, 100);

        // Nothing anywhere -> the built-in default.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.retry_feedback, RetryFeedback::default());
    }

    #[test]
    fn deserializes_partial_retry_feedback_from_harness_json_shape() {
        let json = r#"{ "retry_feedback": { "max_chars": 1500 } }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        let rf = partial
            .retry_feedback
            .as_ref()
            .expect("retry_feedback present");
        assert_eq!(rf.max_chars, Some(1500));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(rf.enabled, None);
    }

    #[test]
    fn merge_call_timeouts_overrides_only_the_fields_it_sets() {
        let base = CallTimeouts {
            implement: Some(600),
            triage: Some(120),
            review: None,
        };
        let over = PartialCallTimeouts {
            implement: Some(900),
            triage: None,
            review: Some(300),
        };
        let merged = merge_call_timeouts(base, &over);
        assert_eq!(merged.implement, Some(900));
        // `None` in the override leaves the base value alone.
        assert_eq!(merged.triage, Some(120));
        assert_eq!(merged.review, Some(300));
    }

    #[test]
    fn timeouts_resolve_through_all_four_layers_in_precedence_order() {
        let harness = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(400),
                triage: Some(200),
                review: Some(100),
            }),
            ..Default::default()
        };
        let profile = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(800),
                triage: Some(500),
                ..Default::default()
            }),
            ..Default::default()
        };
        let event = PartialPolicy {
            timeouts: Some(PartialCallTimeouts {
                implement: Some(1200),
                ..Default::default()
            }),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        // event > profile > harness > builtin(None).
        assert_eq!(resolved.timeouts.implement, Some(1200));
        assert_eq!(resolved.timeouts.triage, Some(500));
        assert_eq!(resolved.timeouts.review, Some(100));

        // Nothing anywhere -> still the built-in all-`None`.
        let untouched = resolve(SdlcPolicy::default(), None, None, None);
        assert_eq!(untouched.timeouts, CallTimeouts::default());
    }

    #[test]
    fn deserializes_partial_call_timeouts_from_harness_json_shape() {
        let json = r#"{ "timeouts": { "implement": 900 } }"#;
        let partial: PartialPolicy = serde_json::from_str(json).expect("valid PartialPolicy JSON");
        let timeouts = partial.timeouts.as_ref().expect("timeouts present");
        assert_eq!(timeouts.implement, Some(900));
        // Absent fields stay `None` and fall through on merge.
        assert_eq!(timeouts.triage, None);
        assert_eq!(timeouts.review, None);
    }

    #[test]
    fn test_depth_serde_wire_contract_is_snake_case() {
        assert_eq!(
            serde_json::to_value(TestDepth::Full).unwrap(),
            serde_json::json!("full")
        );
        assert_eq!(
            serde_json::to_value(TestDepth::Fast).unwrap(),
            serde_json::json!("fast")
        );
        assert_eq!(
            serde_json::from_value::<TestDepth>(serde_json::json!("full")).unwrap(),
            TestDepth::Full
        );
        assert_eq!(
            serde_json::from_value::<TestDepth>(serde_json::json!("fast")).unwrap(),
            TestDepth::Fast
        );
    }

    #[test]
    fn harness_default_overrides_builtin_for_test_depth() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, None);
        assert_eq!(resolved.test_depth, TestDepth::Fast);
    }

    #[test]
    fn profile_overrides_harness_default_for_test_depth() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let profile = PartialPolicy {
            test_depth: Some(TestDepth::Full),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), Some(&profile), None);
        assert_eq!(resolved.test_depth, TestDepth::Full);
    }

    #[test]
    fn event_override_beats_profile_for_test_depth() {
        let profile = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let event = PartialPolicy {
            test_depth: Some(TestDepth::Full),
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), None, Some(&profile), Some(&event));
        assert_eq!(resolved.test_depth, TestDepth::Full);
    }

    #[test]
    fn partial_policy_with_none_test_depth_leaves_lower_layer_untouched() {
        let harness = PartialPolicy {
            test_depth: Some(TestDepth::Fast),
            ..Default::default()
        };
        let event = PartialPolicy {
            test_depth: None,
            ..Default::default()
        };
        let resolved = resolve(SdlcPolicy::default(), Some(&harness), None, Some(&event));
        assert_eq!(resolved.test_depth, TestDepth::Fast);
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

    /// EN.4.0 task 5 step 5.7: pin a fixed non-trivial `(builtin, harness,
    /// profile, event)` input set's resolved-JSON output so a future change
    /// to the delegation onto `crate::policy::resolve` can't silently alter
    /// `SdlcPolicy` resolution — the concrete guard that the refactor
    /// changed nothing observable.
    ///
    /// The `"timeouts":{...all null}` block was appended to this golden when
    /// the per-stage call-timeout knob landed. Every value is `null` — the
    /// new field is present in the serialized shape but carries no override,
    /// so resolution is unchanged for every pre-existing knob.
    ///
    /// The trailing `"retry_feedback":{"enabled":true,"max_chars":4000}`
    /// block was appended when the retry-feedback knob landed. Unlike
    /// `timeouts`, its default is deliberately NOT a no-op (see
    /// `RetryFeedback`'s docs); every pre-existing knob's resolved value in
    /// this golden is nonetheless unchanged.
    #[test]
    fn resolve_output_is_byte_identical_to_pre_hoist_baseline() {
        let harness = PartialPolicy {
            max_attempts: Some(4),
            ..Default::default()
        };
        let cheap_fast = profiles::cheap_fast();
        let event = PartialPolicy {
            llm_triage: Some(true),
            ..Default::default()
        };

        let resolved = resolve(
            SdlcPolicy::default(),
            Some(&harness),
            Some(&cheap_fast),
            Some(&event),
        );

        let expected = "{\"output_verbosity\":\"terse\",\"prompt_cache\":false,\"review_mode\":\"trivial_skip\",\"review_skip_max_files\":2,\"review_skip_max_diff_lines\":40,\"test_depth\":\"fast\",\"model_tiers\":{\"implement\":\"haiku\",\"implement_simple\":\"sonnet\",\"review\":\"local\",\"triage\":\"local\",\"generate\":\"sonnet\"},\"timeouts\":{\"implement\":null,\"triage\":null,\"review\":null},\"local\":{\"endpoint\":\"http://localhost:11434\",\"model\":\"qwen2.5-coder:7b\",\"constrained_json\":false},\"simple_task_max_files\":2,\"llm_triage\":true,\"max_attempts\":4,\"close_out\":{\"reuse\":{\"validation\":false,\"review\":false,\"docs\":false}},\"retry_feedback\":{\"enabled\":true,\"max_chars\":4000}}";

        assert_eq!(serde_json::to_string(&resolved).unwrap(), expected);
    }
}
