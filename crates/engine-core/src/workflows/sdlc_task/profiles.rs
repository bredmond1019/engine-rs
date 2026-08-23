//! Named `PartialSdlcTaskPolicy` profile bundles, the
//! `sdlc_task.{policy,profiles}` `harness.json` section readers, and
//! `resolve_policy_for_run_from` (`EN.11.O` task 2).
//!
//! Modelled on `content_pipeline/profiles.rs` and
//! `approve_and_run/profiles.rs` — the same three-shim shape over the
//! generic `crate::policy::profiles` plumbing, parameterized by
//! [`WORKFLOW_KEY`]. `resolve_policy_for_run_from` also mirrors
//! `sdlc_flow/setup.rs:50-104`'s event-aware variant (it parses the
//! per-run event to pick up `event.profile` / `event.policy`), since
//! SDLC_TASK — unlike `APPROVE_AND_RUN` — has its own trigger event
//! (`SdlcTaskEventSchema`).

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{PartialSdlcTaskPolicy, SdlcTaskPolicy, TestDepth};
use super::schema::SdlcTaskEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`sdlc_task.policy` / `sdlc_task.profiles`) — lives in THIS file,
/// matching the sibling convention (`approve_and_run/profiles.rs:17`,
/// `content_pipeline/profiles.rs:38`), not in `policy.rs`.
pub const WORKFLOW_KEY: &str = "sdlc_task";

/// The explicit control profile: restates the built-in default for every
/// field, so `profile: "baseline"` is a legible, self-documenting no-op.
#[must_use]
pub fn baseline() -> PartialSdlcTaskPolicy {
    let d = SdlcTaskPolicy::default();
    PartialSdlcTaskPolicy {
        output_verbosity: Some(d.output_verbosity),
        prompt_cache: Some(d.prompt_cache),
        // `test_depth: Full` (the built-in default) SILENTLY DISABLES the
        // terminal reconcile — `sdlc_flow/final_validation.rs:265` returns
        // a zero-`CommandRunner`-call passthrough when
        // `policy.test_depth == TestDepth::Full`, on the reasoning that
        // every check already ran authoritative per-task. `baseline` keeps
        // this behaviour unchanged (it IS the control), so its reconcile
        // is skipped exactly as it is today.
        test_depth: Some(d.test_depth),
        model_tiers: Some(super::policy::PartialSdlcTaskModelTiers {
            implement: Some(d.model_tiers.implement),
            triage: Some(d.model_tiers.triage),
            generate: Some(d.model_tiers.generate),
        }),
        timeouts: Some(super::policy::PartialSdlcTaskCallTimeouts {
            implement: d.timeouts.implement,
            triage: d.timeouts.triage,
            generate: d.timeouts.generate,
        }),
        local: Some(crate::policy::PartialLocalConfig::default()),
        llm_triage: Some(d.llm_triage),
        max_attempts: Some(d.max_attempts),
        retry_feedback: Some(super::policy::PartialRetryFeedback {
            enabled: Some(d.retry_feedback.enabled),
            max_chars: Some(d.retry_feedback.max_chars),
        }),
        transport_retry: Some(super::policy::PartialTransportRetry {
            max_attempts: Some(d.transport_retry.max_attempts),
            initial_backoff_ms: Some(d.transport_retry.initial_backoff_ms),
        }),
    }
}

/// Cost/latency floor: `test_depth: Fast` (which, per the caveat above,
/// means this profile is the one that actually RUNS the terminal
/// reconcile — `Full` is what skips it), terse output, prompt caching
/// on, a lower retry ceiling, the cheapest model tier for every
/// model-driven stage, and tighter per-call timeouts.
#[must_use]
pub fn cheap_fast() -> PartialSdlcTaskPolicy {
    PartialSdlcTaskPolicy {
        output_verbosity: Some(super::policy::OutputVerbosity::Terse),
        prompt_cache: Some(true),
        // Fast, not Full — see the module-level caveat: this is the
        // profile that actually exercises `FinalValidationNode`'s
        // reconcile (`sdlc_flow/final_validation.rs:265` only skips on
        // `Full`), unlike `thorough`, which keeps `Full` and skips it.
        test_depth: Some(TestDepth::Fast),
        model_tiers: Some(super::policy::PartialSdlcTaskModelTiers {
            implement: Some(super::policy::ModelTier::Haiku),
            triage: Some(super::policy::ModelTier::Haiku),
            generate: Some(super::policy::ModelTier::Haiku),
        }),
        timeouts: Some(super::policy::PartialSdlcTaskCallTimeouts {
            implement: Some(120),
            triage: Some(60),
            generate: Some(120),
        }),
        local: Some(crate::policy::PartialLocalConfig::default()),
        llm_triage: Some(false),
        max_attempts: Some(2),
        retry_feedback: Some(super::policy::PartialRetryFeedback {
            enabled: Some(true),
            max_chars: Some(2000),
        }),
        transport_retry: Some(super::policy::PartialTransportRetry {
            max_attempts: Some(2),
            initial_backoff_ms: Some(200),
        }),
    }
}

/// Quality ceiling: `test_depth: Full` (per the module-level caveat, this
/// SKIPS the terminal reconcile — every check already ran authoritative
/// per-task, so `thorough` trades the reconcile pass for a higher retry
/// ceiling and the strongest model tier instead), a generous retry
/// ceiling, Opus for the model-driven stages, prompt caching on, and
/// generous per-call timeouts.
#[must_use]
pub fn thorough() -> PartialSdlcTaskPolicy {
    PartialSdlcTaskPolicy {
        output_verbosity: Some(super::policy::OutputVerbosity::Verbose),
        prompt_cache: Some(true),
        // Full, not Fast — see the module-level caveat: `thorough` keeps
        // the built-in `test_depth: Full`, which means
        // `FinalValidationNode`'s reconcile is SKIPPED for this profile,
        // the same way it is skipped today with no profile selected.
        test_depth: Some(TestDepth::Full),
        model_tiers: Some(super::policy::PartialSdlcTaskModelTiers {
            implement: Some(super::policy::ModelTier::Opus),
            triage: Some(super::policy::ModelTier::Sonnet),
            generate: Some(super::policy::ModelTier::Opus),
        }),
        timeouts: Some(super::policy::PartialSdlcTaskCallTimeouts {
            implement: Some(900),
            triage: Some(600),
            generate: Some(900),
        }),
        local: Some(crate::policy::PartialLocalConfig::default()),
        llm_triage: Some(true),
        max_attempts: Some(5),
        retry_feedback: Some(super::policy::PartialRetryFeedback {
            enabled: Some(true),
            max_chars: Some(8000),
        }),
        transport_retry: Some(super::policy::PartialTransportRetry {
            max_attempts: Some(5),
            initial_backoff_ms: Some(500),
        }),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns
/// `None` for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialSdlcTaskPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Read `sdlc_task.policy` (a [`PartialSdlcTaskPolicy`]) out of the file
/// addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from`, parameterized by
/// [`WORKFLOW_KEY`].
pub fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialSdlcTaskPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve a named `profile` to a [`PartialSdlcTaskPolicy`] bundle,
/// preferring a `harness.json` `sdlc_task.profiles[name]` entry (read via
/// `source`) over the built-in [`profile_by_name`]. Returns `Ok(None)`
/// when `profile_name` is `None`, and `Err` when a name is given but
/// resolves to neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from`, parameterized by [`WORKFLOW_KEY`]
/// and [`profile_by_name`].
pub fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialSdlcTaskPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Parse `ctx.event` into a [`SdlcTaskEventSchema`].
fn parse_event(ctx: &TaskContext) -> Result<SdlcTaskEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid SDLC_TASK event: {err}")))
}

/// Resolve the four-layer [`SdlcTaskPolicy`] for this run against an
/// arbitrary [`PolicyConfigSource`]: the inbound event's `policy`
/// override, the resolved `profile` bundle, `source`'s `sdlc_task.policy`
/// defaults, and the built-in default, high->low precedence via
/// [`super::policy::resolve`]. Mirrors `sdlc_flow::setup::
/// resolve_policy_for_run_from` (`setup.rs:83-96`).
///
/// `event.policy` is [`PartialSdlcTaskPolicy`] directly (`EN.11.O` task 3
/// narrowed `SdlcTaskEventSchema::policy` off the placeholder
/// `Option<serde_json::Value>`), so no extra parse step is needed here.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<SdlcTaskPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    let event_override = event.policy.clone();
    Ok(super::policy::resolve(
        SdlcTaskPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event_override.as_ref(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_key_is_sdlc_task() {
        assert_eq!(WORKFLOW_KEY, "sdlc_task");
    }

    #[test]
    fn profile_by_name_resolves_all_three_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    /// Standing rule 6: "a knob absent from the profile bundles is a knob
    /// nobody will find." Asserted mechanically over the serialized JSON
    /// key set of `PartialSdlcTaskPolicy`'s own default (all keys present,
    /// all `null`) against each profile's serialized shape, so a field
    /// added to `PartialSdlcTaskPolicy` later with no profile update fails
    /// this test rather than silently shipping unset.
    #[test]
    fn every_named_profile_sets_every_top_level_knob() {
        let all_keys: Vec<String> = serde_json::to_value(PartialSdlcTaskPolicy::default())
            .unwrap()
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert!(!all_keys.is_empty());

        for (name, profile) in [
            ("baseline", baseline()),
            ("cheap-fast", cheap_fast()),
            ("thorough", thorough()),
        ] {
            let json = serde_json::to_value(&profile).unwrap();
            let obj = json.as_object().unwrap();
            for key in &all_keys {
                assert!(
                    obj.get(key).is_some_and(|v| !v.is_null()),
                    "profile `{name}` leaves knob `{key}` unset (null)"
                );
            }
        }
    }

    #[test]
    fn baseline_projects_to_the_behavior_stable_default() {
        let resolved =
            super::super::policy::resolve(SdlcTaskPolicy::default(), None, Some(&baseline()), None);
        assert_eq!(resolved, SdlcTaskPolicy::default());
    }

    #[test]
    fn cheap_fast_sets_the_documented_cost_latency_floor() {
        let p = cheap_fast();
        assert_eq!(p.test_depth, Some(TestDepth::Fast));
        assert_eq!(p.max_attempts, Some(2));
        assert_eq!(
            p.model_tiers.unwrap().implement,
            Some(super::super::policy::ModelTier::Haiku)
        );
    }

    #[test]
    fn thorough_sets_the_documented_quality_ceiling() {
        let p = thorough();
        assert_eq!(p.test_depth, Some(TestDepth::Full));
        assert_eq!(p.max_attempts, Some(5));
        assert_eq!(
            p.model_tiers.unwrap().implement,
            Some(super::super::policy::ModelTier::Opus)
        );
    }

    /// The trap this whole module documents twice (`cheap_fast`,
    /// `thorough`): `test_depth: Full` (kept by `thorough`) is what SKIPS
    /// the reconcile, while `cheap_fast`'s `Fast` is what runs it.
    #[test]
    fn thorough_keeps_full_test_depth_and_cheap_fast_switches_to_fast() {
        assert_eq!(thorough().test_depth, Some(TestDepth::Full));
        assert_eq!(cheap_fast().test_depth, Some(TestDepth::Fast));
    }

    fn event_context(body: serde_json::Value) -> TaskContext {
        TaskContext {
            event: body,
            nodes: Default::default(),
            metadata: serde_json::json!({}),
            node_runs: Default::default(),
        }
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_filesystem() {
        let ctx = event_context(serde_json::json!({"spec_slug": "X"}));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, SdlcTaskPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_applies_named_profile_from_event() {
        let ctx = event_context(serde_json::json!({
            "spec_slug": "X",
            "profile": "cheap-fast"
        }));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 2);
        assert_eq!(resolved.test_depth, TestDepth::Fast);
    }

    #[test]
    fn resolve_policy_for_run_from_unknown_profile_errors() {
        let ctx = event_context(serde_json::json!({
            "spec_slug": "X",
            "profile": "nonexistent"
        }));
        let err = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_event_policy_override_beats_profile() {
        let ctx = event_context(serde_json::json!({
            "spec_slug": "X",
            "profile": "thorough",
            "policy": {"max_attempts": 9}
        }));
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 9);
    }

    #[test]
    fn resolve_policy_for_run_from_reads_harness_file_source() {
        let dir = std::env::temp_dir().join(format!(
            "engine-sdlc-task-profiles-test-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let harness_file = dir.join("harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "sdlc_task": { "policy": { "max_attempts": 4 } }
            })
            .to_string(),
        )
        .expect("write harness file");

        let source = PolicyConfigSource::HarnessFile(harness_file);
        let ctx = event_context(serde_json::json!({"spec_slug": "X"}));
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        assert_eq!(resolved.max_attempts, 4);
        // Untouched knob falls through to builtin.
        assert_eq!(resolved.test_depth, TestDepth::Full);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
