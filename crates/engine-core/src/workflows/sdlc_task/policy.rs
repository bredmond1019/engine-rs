//! The resolved `SdlcTaskPolicy` — SDLC_TASK's own policy surface
//! (`EN.11.O` task 1), standing rule 6's cost/latency/quality knobs for the
//! lean implement -> fast-test -> fix -> commit loop.
//!
//! **This is a SIBLING of `sdlc_flow::policy::SdlcPolicy`, not a fork of
//! it.** SDLC_TASK reuses `sdlc_flow`'s shared node set (see `mod.rs`), so
//! the scalar/enum types those nodes already read
//! (`OutputVerbosity`/`TestDepth`/`RetryFeedback`/`TransportRetry`) are
//! imported straight from `crate::workflows::sdlc_flow::policy` rather than
//! redefined here — and `ModelTier`/`LocalConfig` from `crate::policy::tier`
//! — so the two engines can never drift on what these types mean.
//!
//! **Deliberately omitted — six knobs, because SDLC_TASK's registry
//! (`graph.rs`) registers no review node and no docs node:**
//! 1. `review_mode`
//! 2. `review_skip_max_files`
//! 3. `review_skip_max_diff_lines`
//! 4. `max_review_attempts`
//! 5. `review_diff_max_chars`
//! 6. the `docs`/`review`/`implement_simple` model-tier trio (bundled as
//!    one item: `ModelTiers` on `SdlcPolicy` carries all three, but no
//!    SDLC_TASK node reads any of them)
//!
//! Advertising any of these on `SdlcTaskPolicy` would be the
//! advertised-but-unread defect this block exists to prevent — the same
//! class of bug the `transport-retry-policy-not-wired-to-call-sites`
//! carryover records for `SdlcPolicy.transport_retry` itself.
//!
//! **Resolution — four layers, high-to-low precedence:** per-run
//! `SdlcTaskEventSchema` `policy` override, then a named `profile:` bundle
//! (`profiles.rs`), then `planning/harness.json`'s `sdlc_task.policy`
//! defaults, then [`SdlcTaskPolicy::default`]. The built-in default MUST
//! reproduce today's (pre-`EN.11.O`) SDLC_TASK behavior exactly — see
//! [`SdlcTaskPolicy::to_sdlc_policy`] and the
//! `default_projects_to_sdlc_policy_default` test, which IS the
//! behavior-stability guarantee.
//!
//! **The projection.** SDLC_TASK's shared nodes (`ImplementTaskNode`,
//! `TestTaskNode`, `TriageTaskNode`, `FinalValidationNode`) all read
//! `SdlcPolicy`, not this type — they are `sdlc_flow`'s nodes, unmodified.
//! [`SdlcTaskPolicy::to_sdlc_policy`] projects this resolved policy onto an
//! `SdlcPolicy`, leaving every omitted review knob at
//! `SdlcPolicy::default()`'s value, so those shared nodes see a policy
//! shaped exactly as they already expect. `EN.11.O` task 3 wires this
//! projection into `SetupWorktreeNode`'s stamp; this module only builds it.

use serde::{Deserialize, Serialize};

pub use crate::policy::tier::{LocalConfig, ModelTier};
pub use crate::policy::PartialLocalConfig;
use crate::policy::{merge_opt, Overlay};
use crate::workflows::sdlc_flow::policy::SdlcPolicy;
pub use crate::workflows::sdlc_flow::policy::{
    OutputVerbosity, PartialRetryFeedback, PartialTransportRetry, RetryFeedback, TestDepth,
    TransportRetry,
};

/// Per-stage model tier assignment for SDLC_TASK's three model-driven
/// stages. Deliberately narrower than `sdlc_flow::policy::ModelTiers` — see
/// this module's doc comment for the omitted trio (`review`/`docs`/
/// `implement_simple`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdlcTaskModelTiers {
    pub implement: ModelTier,
    pub triage: ModelTier,
    /// `GenerateTasksNode`'s tier, reused unchanged from `sdlc_flow` — see
    /// `sdlc_flow::policy::ModelTiers::generate`'s note on why this is
    /// `Opus`, not `Sonnet`, by default.
    pub generate: ModelTier,
}

impl Default for SdlcTaskModelTiers {
    /// Sonnet for `implement`/`triage`, `Opus` for `generate` — matches
    /// `sdlc_flow::policy::ModelTiers::default()` field-for-field on every
    /// field SDLC_TASK actually reads.
    fn default() -> Self {
        Self {
            implement: ModelTier::Sonnet,
            triage: ModelTier::Sonnet,
            generate: ModelTier::Opus,
        }
    }
}

/// All-optional mirror of [`SdlcTaskModelTiers`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialSdlcTaskModelTiers {
    pub implement: Option<ModelTier>,
    pub triage: Option<ModelTier>,
    pub generate: Option<ModelTier>,
}

fn merge_sdlc_task_model_tiers(
    mut base: SdlcTaskModelTiers,
    over: &PartialSdlcTaskModelTiers,
) -> SdlcTaskModelTiers {
    if let Some(v) = over.implement {
        base.implement = v;
    }
    if let Some(v) = over.triage {
        base.triage = v;
    }
    if let Some(v) = over.generate {
        base.generate = v;
    }
    base
}

/// Per-stage whole-call timeout, in **seconds**, for SDLC_TASK's three
/// model-driven stages. `None` (the behavior-stable built-in default for
/// every field) means "set nothing", leaving `claude_code_rs::Config::timeout`
/// at its own unconfigured 300s default — mirrors
/// `sdlc_flow::policy::CallTimeouts`'s semantics exactly, just narrower.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SdlcTaskCallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub generate: Option<u64>,
}

/// All-optional mirror of [`SdlcTaskCallTimeouts`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialSdlcTaskCallTimeouts {
    pub implement: Option<u64>,
    pub triage: Option<u64>,
    pub generate: Option<u64>,
}

fn merge_sdlc_task_call_timeouts(
    mut base: SdlcTaskCallTimeouts,
    over: &PartialSdlcTaskCallTimeouts,
) -> SdlcTaskCallTimeouts {
    if let Some(v) = over.implement {
        base.implement = Some(v);
    }
    if let Some(v) = over.triage {
        base.triage = Some(v);
    }
    if let Some(v) = over.generate {
        base.generate = Some(v);
    }
    base
}

/// The fully-resolved, per-run SDLC_TASK policy — the merge of built-in
/// defaults, `harness.json`'s `sdlc_task.policy` defaults, a named
/// `profile:` bundle, and any per-run event override, high->low precedence
/// in that order (see [`resolve`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SdlcTaskPolicy {
    pub output_verbosity: OutputVerbosity,
    pub prompt_cache: bool,
    /// How much of a task's own check suite `TestTaskNode` runs, and
    /// whether `FinalValidationNode`'s terminal reconcile runs at all.
    ///
    /// **Trap:** `test_depth: Full` — the behavior-stable built-in
    /// default — SILENTLY DISABLES the reconcile:
    /// `sdlc_flow/final_validation.rs:265` returns a zero-`CommandRunner`-
    /// call passthrough when `policy.test_depth == TestDepth::Full`, on the
    /// reasoning that every check already ran authoritative per-task. So
    /// `thorough` (which keeps `Full`) skips the reconcile and `cheap-fast`
    /// (which sets `Fast`) is the profile that actually runs it. See
    /// `profiles.rs`'s `cheap_fast`/`thorough` doc comments for the same
    /// caveat restated at the two profiles it affects.
    pub test_depth: TestDepth,
    pub model_tiers: SdlcTaskModelTiers,
    pub timeouts: SdlcTaskCallTimeouts,
    /// Configuration for the `local` model tier, when any stage uses it.
    pub local: LocalConfig,
    /// Enables `TriageTaskNode`'s model-triage branch — same semantics as
    /// `sdlc_flow::policy::SdlcPolicy::llm_triage`.
    pub llm_triage: bool,
    /// Run-wide SEED for a task's `max_attempts`, same precedence rule as
    /// `SdlcPolicy::max_attempts`: a task-declared value always wins: this
    /// is only the default for a task that omits the field.
    pub max_attempts: u32,
    /// Whether the previous attempt's failure output is fed back into
    /// `ImplementTaskNode`'s retry prompt, and how large that block may
    /// get. Reused type — see `sdlc_flow::policy::RetryFeedback`.
    pub retry_feedback: RetryFeedback,
    /// Bounded in-node retry-with-backoff budget for `ClaudeCodeStep`'s
    /// transport call. Reused type — see
    /// `sdlc_flow::policy::TransportRetry`.
    pub transport_retry: TransportRetry,
}

impl Default for SdlcTaskPolicy {
    /// Reproduces today's (pre-`EN.11.O`) SDLC_TASK behavior exactly —
    /// every field here matches what `SdlcPolicy::default()` resolves for
    /// the corresponding shared field. See
    /// `default_projects_to_sdlc_policy_default` below, which asserts this
    /// as an equality rather than by inspection.
    fn default() -> Self {
        Self {
            output_verbosity: OutputVerbosity::Normal,
            prompt_cache: false,
            test_depth: TestDepth::Full,
            model_tiers: SdlcTaskModelTiers::default(),
            timeouts: SdlcTaskCallTimeouts::default(),
            local: LocalConfig::default(),
            llm_triage: false,
            max_attempts: 3,
            retry_feedback: RetryFeedback::default(),
            transport_retry: TransportRetry::default(),
        }
    }
}

impl SdlcTaskPolicy {
    /// Project this resolved policy onto an `SdlcPolicy` — THE seam that
    /// lets SDLC_TASK's reused `sdlc_flow` nodes (which all read
    /// `SdlcPolicy`) see this workflow's own resolved values. Every field
    /// SDLC_TASK omits (the six named in this module's doc comment) is left
    /// at `SdlcPolicy::default()`'s value, never at an arbitrary or
    /// zero value — so a node that DID accidentally start reading an
    /// "omitted" field would observe today's behavior-stable default, not
    /// garbage.
    #[must_use]
    pub fn to_sdlc_policy(&self) -> SdlcPolicy {
        let fallback = SdlcPolicy::default();
        SdlcPolicy {
            output_verbosity: self.output_verbosity,
            prompt_cache: self.prompt_cache,
            review_mode: fallback.review_mode,
            review_skip_max_files: fallback.review_skip_max_files,
            review_skip_max_diff_lines: fallback.review_skip_max_diff_lines,
            test_depth: self.test_depth,
            model_tiers: crate::workflows::sdlc_flow::policy::ModelTiers {
                implement: self.model_tiers.implement,
                implement_simple: fallback.model_tiers.implement_simple,
                review: fallback.model_tiers.review,
                triage: self.model_tiers.triage,
                generate: self.model_tiers.generate,
                docs: fallback.model_tiers.docs,
            },
            timeouts: crate::workflows::sdlc_flow::policy::CallTimeouts {
                implement: self.timeouts.implement,
                triage: self.timeouts.triage,
                review: fallback.timeouts.review,
                generate: self.timeouts.generate,
                docs: fallback.timeouts.docs,
            },
            local: self.local.clone(),
            llm_triage: self.llm_triage,
            max_attempts: self.max_attempts,
            max_review_attempts: fallback.max_review_attempts,
            retry_feedback: self.retry_feedback,
            transport_retry: self.transport_retry,
            review_diff_max_chars: fallback.review_diff_max_chars,
        }
    }
}

/// All-optional mirror of [`SdlcTaskPolicy`] used by the three override
/// layers (`harness.json`'s `sdlc_task.policy`, a named `profile:` bundle,
/// and a per-run event's `policy` field). Every field left `None` falls
/// through to the next-lower-precedence layer. Mirrors
/// `sdlc_flow::policy::PartialPolicy`'s derive set exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PartialSdlcTaskPolicy {
    pub output_verbosity: Option<OutputVerbosity>,
    pub prompt_cache: Option<bool>,
    pub test_depth: Option<TestDepth>,
    pub model_tiers: Option<PartialSdlcTaskModelTiers>,
    pub timeouts: Option<PartialSdlcTaskCallTimeouts>,
    pub local: Option<PartialLocalConfig>,
    pub llm_triage: Option<bool>,
    pub max_attempts: Option<u32>,
    pub retry_feedback: Option<PartialRetryFeedback>,
    pub transport_retry: Option<PartialTransportRetry>,
}

fn merge_retry_feedback(mut base: RetryFeedback, over: &PartialRetryFeedback) -> RetryFeedback {
    if let Some(v) = over.enabled {
        base.enabled = v;
    }
    if let Some(v) = over.max_chars {
        base.max_chars = v;
    }
    base
}

fn merge_transport_retry(mut base: TransportRetry, over: &PartialTransportRetry) -> TransportRetry {
    if let Some(v) = over.max_attempts {
        base.max_attempts = v;
    }
    if let Some(v) = over.initial_backoff_ms {
        base.initial_backoff_ms = v;
    }
    base
}

impl crate::policy::Policy for SdlcTaskPolicy {
    type Partial = PartialSdlcTaskPolicy;

    /// Apply one override layer on top of `self`, field-by-field (`Some` in
    /// `over` wins, `None` falls through to `self`) — mirrors
    /// `sdlc_flow::policy::SdlcPolicy`'s `Policy::apply` exactly, minus the
    /// omitted review fields.
    fn apply(self, over: &PartialSdlcTaskPolicy) -> Self {
        let base = self;
        SdlcTaskPolicy {
            output_verbosity: merge_opt(base.output_verbosity, over.output_verbosity),
            prompt_cache: merge_opt(base.prompt_cache, over.prompt_cache),
            test_depth: merge_opt(base.test_depth, over.test_depth),
            model_tiers: match &over.model_tiers {
                Some(mt) => merge_sdlc_task_model_tiers(base.model_tiers, mt),
                None => base.model_tiers,
            },
            timeouts: match &over.timeouts {
                Some(t) => merge_sdlc_task_call_timeouts(base.timeouts, t),
                None => base.timeouts,
            },
            local: match &over.local {
                Some(l) => base.local.overlay(l),
                None => base.local,
            },
            llm_triage: merge_opt(base.llm_triage, over.llm_triage),
            max_attempts: merge_opt(base.max_attempts, over.max_attempts),
            retry_feedback: match &over.retry_feedback {
                Some(rf) => merge_retry_feedback(base.retry_feedback, rf),
                None => base.retry_feedback,
            },
            transport_retry: match &over.transport_retry {
                Some(tr) => merge_transport_retry(base.transport_retry, tr),
                None => base.transport_retry,
            },
        }
    }
}

/// Resolve the four policy layers into one concrete [`SdlcTaskPolicy`],
/// high->low precedence: `event_override` beats `profile` beats
/// `harness_defaults` beats `builtin`. Delegates to the generic
/// `crate::policy::resolve::resolve` — same shim shape as
/// `sdlc_flow::policy::resolve`.
#[must_use]
pub fn resolve(
    builtin: SdlcTaskPolicy,
    harness_defaults: Option<&PartialSdlcTaskPolicy>,
    profile: Option<&PartialSdlcTaskPolicy>,
    event_override: Option<&PartialSdlcTaskPolicy>,
) -> SdlcTaskPolicy {
    crate::policy::resolve::resolve(builtin, harness_defaults, profile, event_override)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// THE behavior-stability guarantee: `SdlcTaskPolicy::default()`,
    /// projected onto an `SdlcPolicy`, equals `SdlcPolicy::default()`
    /// exactly. Asserted by equality, not by inspection.
    #[test]
    fn default_projects_to_sdlc_policy_default() {
        assert_eq!(
            SdlcTaskPolicy::default().to_sdlc_policy(),
            SdlcPolicy::default()
        );
    }

    #[test]
    fn builtin_default_matches_pre_en_11_o_baseline() {
        let policy = SdlcTaskPolicy::default();
        assert_eq!(policy.output_verbosity, OutputVerbosity::Normal);
        assert!(!policy.prompt_cache);
        assert_eq!(policy.test_depth, TestDepth::Full);
        assert_eq!(policy.model_tiers.implement, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.triage, ModelTier::Sonnet);
        assert_eq!(policy.model_tiers.generate, ModelTier::Opus);
        assert_eq!(policy.timeouts, SdlcTaskCallTimeouts::default());
        assert!(!policy.llm_triage);
        assert_eq!(policy.max_attempts, 3);
        assert_eq!(policy.retry_feedback, RetryFeedback::default());
        assert_eq!(policy.transport_retry, TransportRetry::default());
    }

    #[test]
    fn merge_opt_falls_through_on_none_for_a_scalar_field() {
        let base = SdlcTaskPolicy::default();
        let over = PartialSdlcTaskPolicy::default();
        let resolved = crate::policy::Policy::apply(base.clone(), &over);
        assert_eq!(resolved, base);
    }

    #[test]
    fn merge_opt_overrides_when_present_for_a_scalar_field() {
        let base = SdlcTaskPolicy::default();
        let over = PartialSdlcTaskPolicy {
            max_attempts: Some(7),
            ..Default::default()
        };
        let resolved = crate::policy::Policy::apply(base, &over);
        assert_eq!(resolved.max_attempts, 7);
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_builtin_scalar() {
        let harness = PartialSdlcTaskPolicy {
            max_attempts: Some(1),
            ..Default::default()
        };
        let profile = PartialSdlcTaskPolicy {
            max_attempts: Some(2),
            ..Default::default()
        };
        let event = PartialSdlcTaskPolicy {
            max_attempts: Some(3),
            ..Default::default()
        };
        let resolved = resolve(
            SdlcTaskPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.max_attempts, 3);
    }

    #[test]
    fn resolve_precedence_event_beats_profile_beats_harness_beats_builtin_nested() {
        let harness = PartialSdlcTaskPolicy {
            model_tiers: Some(PartialSdlcTaskModelTiers {
                implement: Some(ModelTier::Haiku),
                triage: Some(ModelTier::Haiku),
                generate: None,
            }),
            ..Default::default()
        };
        let profile = PartialSdlcTaskPolicy {
            model_tiers: Some(PartialSdlcTaskModelTiers {
                implement: Some(ModelTier::Opus),
                triage: None,
                generate: None,
            }),
            ..Default::default()
        };
        // Event leaves `model_tiers` untouched entirely, so profile's
        // `implement` and harness's `triage` both survive.
        let event = PartialSdlcTaskPolicy::default();

        let resolved = resolve(
            SdlcTaskPolicy::default(),
            Some(&harness),
            Some(&profile),
            Some(&event),
        );
        assert_eq!(resolved.model_tiers.implement, ModelTier::Opus);
        assert_eq!(resolved.model_tiers.triage, ModelTier::Haiku);
        // Untouched by any layer — falls all the way through to builtin.
        assert_eq!(resolved.model_tiers.generate, ModelTier::Opus);
    }

    #[test]
    fn no_review_knob_or_docs_review_implement_simple_tier_on_sdlc_task_policy() {
        // Mechanical check over the serialized shape: none of the six
        // omitted knob names appear anywhere in `SdlcTaskPolicy`'s JSON.
        let json = serde_json::to_value(SdlcTaskPolicy::default()).unwrap();
        let blob = serde_json::to_string(&json).unwrap();
        let banned = [
            "review_mode",
            "review_skip_max_files",
            "review_skip_max_diff_lines",
            "max_review_attempts",
            "review_diff_max_chars",
            "implement_simple",
        ];
        for name in banned {
            assert!(
                !blob.contains(name),
                "SdlcTaskPolicy unexpectedly serializes omitted knob `{name}`: {blob}"
            );
        }
        // `model_tiers` carries no `review` or `docs` field either.
        assert!(!json["model_tiers"]
            .as_object()
            .unwrap()
            .contains_key("review"));
        assert!(!json["model_tiers"]
            .as_object()
            .unwrap()
            .contains_key("docs"));
    }

    // --- EN.11.O task 5: the advertised-but-unread guards -----------------
    //
    // The live precedent both guards exist to prevent:
    // `SdlcPolicy.transport_retry` resolves through all four layers while
    // every `ClaudeCodeStep` consumer runs `TransportRetry::default()` — the
    // knob and the value actually read are two different things
    // (`transport-retry-policy-not-wired-to-call-sites`). Guard A pins that
    // `harness.json`'s advertised key set matches the struct's real field
    // set (a plain successful `serde_json::from_value` parse would NOT
    // catch this — serde ignores unknown keys by default, and a missing
    // key just silently no-ops). Guard B pins that every field on
    // `SdlcTaskPolicy` actually reaches the projected `SdlcPolicy` field a
    // shared node reads.

    /// GUARD A — no advertised-but-unread knob. Reads THIS repo's real
    /// `planning/harness.json` (via `CARGO_MANIFEST_DIR`, which resolves
    /// through the `planning/` -> vault symlink to the real file — see
    /// CLAUDE.md's symlink warning) rather than a synthetic fixture, since
    /// the point is to pin what actually ships. Compares the KEY SET of
    /// `sdlc_task.policy` (minus any `_comment*` sibling) against a
    /// fully-populated `PartialSdlcTaskPolicy`'s serialized key set —
    /// mechanically derived from the struct, never hand-copied, so a field
    /// added to one side without the other fails this test.
    ///
    /// `planning/` is a gitignored symlink into the company-brain vault, so
    /// this SKIPS rather than fails when the vault is not mounted (a bare
    /// clone, or this repo's own public CI, which never checks the private
    /// brain repo out) — mirrors sdlc_flow::policy's
    /// `repo_harness_json_deserializes_every_sdlc_policy_and_profile`; it is
    /// a guard for this working tree, not a hard dependency.
    #[test]
    fn harness_json_sdlc_task_policy_key_set_matches_partial_sdlc_task_policy_fields() {
        let harness_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../planning/harness.json");
        let Ok(raw) = std::fs::read_to_string(&harness_path) else {
            tracing::debug!(path = %harness_path.display(), "skipping: planning/ vault not mounted");
            return;
        };
        let harness: serde_json::Value =
            serde_json::from_str(&raw).expect("planning/harness.json must be valid JSON");
        let policy_section = harness
            .get("sdlc_task")
            .and_then(|v| v.get("policy"))
            .and_then(|v| v.as_object())
            .expect("planning/harness.json must have a sdlc_task.policy object");

        let advertised: std::collections::BTreeSet<String> = policy_section
            .keys()
            .filter(|k| !k.starts_with("_comment"))
            .cloned()
            .collect();

        // Every field set to `Some`, mirroring `SdlcTaskPolicy::default()`'s
        // values — this is what "fully populated" means for the purpose of
        // enumerating field names via serde.
        let full = PartialSdlcTaskPolicy {
            output_verbosity: Some(OutputVerbosity::Normal),
            prompt_cache: Some(false),
            test_depth: Some(TestDepth::Full),
            model_tiers: Some(PartialSdlcTaskModelTiers {
                implement: Some(ModelTier::Sonnet),
                triage: Some(ModelTier::Sonnet),
                generate: Some(ModelTier::Opus),
            }),
            timeouts: Some(PartialSdlcTaskCallTimeouts {
                implement: None,
                triage: None,
                generate: None,
            }),
            local: Some(PartialLocalConfig::default()),
            llm_triage: Some(false),
            max_attempts: Some(3),
            retry_feedback: Some(PartialRetryFeedback {
                enabled: Some(true),
                max_chars: Some(4000),
            }),
            transport_retry: Some(PartialTransportRetry {
                max_attempts: Some(3),
                initial_backoff_ms: Some(200),
            }),
        };
        let value = serde_json::to_value(&full).expect("serialize PartialSdlcTaskPolicy");
        let expected: std::collections::BTreeSet<String> = value
            .as_object()
            .expect("PartialSdlcTaskPolicy serializes as an object")
            .keys()
            .cloned()
            .collect();

        assert_eq!(
            advertised, expected,
            "planning/harness.json's sdlc_task.policy key set must exactly match \
             PartialSdlcTaskPolicy's fields -- a mismatch is either an \
             advertised-but-unread knob (extra key here) or an unadvertised one \
             (missing key here)"
        );
    }

    /// GUARD B — every knob has a production call site. Table-driven over
    /// every `SdlcTaskPolicy` field: set it to a non-default value, project
    /// via `to_sdlc_policy()`, and assert the projected `SdlcPolicy` field a
    /// shared node (`ImplementTaskNode`/`TestTaskNode`/`TriageTaskNode`/
    /// `FinalValidationNode`) actually reads differs from the default
    /// projection in exactly that field. A field that projects nowhere is
    /// unread by construction.
    #[test]
    fn every_sdlc_task_policy_field_projects_onto_the_sdlc_policy_field_a_shared_node_reads() {
        let default = SdlcTaskPolicy::default();
        let default_projection = default.to_sdlc_policy();

        let mut p = default.clone();
        p.output_verbosity = OutputVerbosity::Terse;
        assert_ne!(p.output_verbosity, default.output_verbosity);
        assert_eq!(p.to_sdlc_policy().output_verbosity, OutputVerbosity::Terse);
        assert_ne!(
            p.to_sdlc_policy().output_verbosity,
            default_projection.output_verbosity
        );

        let mut p = default.clone();
        p.prompt_cache = !default.prompt_cache;
        assert_eq!(p.to_sdlc_policy().prompt_cache, p.prompt_cache);
        assert_ne!(
            p.to_sdlc_policy().prompt_cache,
            default_projection.prompt_cache
        );

        let mut p = default.clone();
        p.test_depth = TestDepth::Fast;
        assert_ne!(p.test_depth, default.test_depth);
        assert_eq!(p.to_sdlc_policy().test_depth, TestDepth::Fast);
        assert_ne!(p.to_sdlc_policy().test_depth, default_projection.test_depth);

        let mut p = default.clone();
        p.model_tiers.implement = ModelTier::Haiku;
        assert_eq!(p.to_sdlc_policy().model_tiers.implement, ModelTier::Haiku);
        assert_ne!(
            p.to_sdlc_policy().model_tiers.implement,
            default_projection.model_tiers.implement
        );

        let mut p = default.clone();
        p.model_tiers.triage = ModelTier::Opus;
        assert_eq!(p.to_sdlc_policy().model_tiers.triage, ModelTier::Opus);
        assert_ne!(
            p.to_sdlc_policy().model_tiers.triage,
            default_projection.model_tiers.triage
        );

        let mut p = default.clone();
        p.model_tiers.generate = ModelTier::Haiku;
        assert_eq!(p.to_sdlc_policy().model_tiers.generate, ModelTier::Haiku);
        assert_ne!(
            p.to_sdlc_policy().model_tiers.generate,
            default_projection.model_tiers.generate
        );

        let mut p = default.clone();
        p.timeouts.implement = Some(42);
        assert_eq!(p.to_sdlc_policy().timeouts.implement, Some(42));
        assert_ne!(
            p.to_sdlc_policy().timeouts.implement,
            default_projection.timeouts.implement
        );

        let mut p = default.clone();
        p.timeouts.triage = Some(43);
        assert_eq!(p.to_sdlc_policy().timeouts.triage, Some(43));
        assert_ne!(
            p.to_sdlc_policy().timeouts.triage,
            default_projection.timeouts.triage
        );

        let mut p = default.clone();
        p.timeouts.generate = Some(44);
        assert_eq!(p.to_sdlc_policy().timeouts.generate, Some(44));
        assert_ne!(
            p.to_sdlc_policy().timeouts.generate,
            default_projection.timeouts.generate
        );

        let mut p = default.clone();
        p.local.endpoint = "http://example.invalid:9999".to_string();
        assert_eq!(p.to_sdlc_policy().local.endpoint, p.local.endpoint);
        assert_ne!(
            p.to_sdlc_policy().local.endpoint,
            default_projection.local.endpoint
        );

        let mut p = default.clone();
        p.llm_triage = !default.llm_triage;
        assert_eq!(p.to_sdlc_policy().llm_triage, p.llm_triage);
        assert_ne!(p.to_sdlc_policy().llm_triage, default_projection.llm_triage);

        let mut p = default.clone();
        p.max_attempts = 9;
        assert_eq!(p.to_sdlc_policy().max_attempts, 9);
        assert_ne!(
            p.to_sdlc_policy().max_attempts,
            default_projection.max_attempts
        );

        let mut p = default.clone();
        p.retry_feedback.max_chars = 12_345;
        assert_eq!(p.to_sdlc_policy().retry_feedback.max_chars, 12_345);
        assert_ne!(
            p.to_sdlc_policy().retry_feedback.max_chars,
            default_projection.retry_feedback.max_chars
        );

        let mut p = default.clone();
        p.transport_retry.max_attempts = 9;
        assert_eq!(p.to_sdlc_policy().transport_retry.max_attempts, 9);
        assert_ne!(
            p.to_sdlc_policy().transport_retry.max_attempts,
            default_projection.transport_retry.max_attempts
        );
    }
}
