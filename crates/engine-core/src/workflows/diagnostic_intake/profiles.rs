//! Named `PartialDiagnosticIntakePolicy` profile bundles, the
//! `diagnostic_intake.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run`.
//!
//! Mirrors `research_agent::profiles` / `sdlc_flow::profiles` (the
//! named-bundle catalog) generalized over the shared
//! `crate::policy::profiles` plumbing (EN.4.0). There is no setup node in
//! this workflow — the single terminal node (`IntakeExtractNode`) calls
//! [`resolve_policy_for_run`] directly.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{
    self, DiagnosticIntakePolicy, ModelTier, OutputVerbosity, PartialDiagnosticIntakePolicy,
    PartialLocalConfig, PartialModelTiers,
};
use super::schema::DiagnosticIntakeEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`diagnostic_intake.policy` / `diagnostic_intake.profiles`),
/// passed to the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "diagnostic_intake";

/// The explicit control profile: Sonnet on `extract`, normal verbosity,
/// prompt cache off. Spelled out explicitly (rather than left all-`None`)
/// so selecting `profile: "baseline"` is a legible, self-documenting no-op
/// against the built-in default.
#[must_use]
pub fn baseline() -> PartialDiagnosticIntakePolicy {
    PartialDiagnosticIntakePolicy {
        output_verbosity: Some(OutputVerbosity::Normal),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            extract: Some(ModelTier::Sonnet),
        }),
        local: None,
    }
}

/// Cheapest/fastest cloud profile: `haiku` extract, terse output, prompt
/// caching on.
#[must_use]
pub fn cheap_fast() -> PartialDiagnosticIntakePolicy {
    PartialDiagnosticIntakePolicy {
        output_verbosity: Some(OutputVerbosity::Terse),
        prompt_cache: Some(true),
        model_tiers: Some(PartialModelTiers {
            extract: Some(ModelTier::Haiku),
        }),
        local: None,
    }
}

/// Highest-quality profile: `opus` extract, verbose output.
#[must_use]
pub fn thorough() -> PartialDiagnosticIntakePolicy {
    PartialDiagnosticIntakePolicy {
        output_verbosity: Some(OutputVerbosity::Verbose),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            extract: Some(ModelTier::Opus),
        }),
        local: None,
    }
}

/// Local-extraction profile: rewires the `extract` stage to the
/// OpenAI-compat local transport (`graph::registry_for_policy`) with
/// schema-constrained decoding on. Exercises the Local-tier rewire that is
/// the key difference between this workflow and `research_agent`.
#[must_use]
pub fn local_extract() -> PartialDiagnosticIntakePolicy {
    PartialDiagnosticIntakePolicy {
        output_verbosity: Some(OutputVerbosity::Normal),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            extract: Some(ModelTier::Local),
        }),
        local: Some(PartialLocalConfig {
            endpoint: None,
            model: None,
            constrained_json: Some(true),
        }),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the four canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialDiagnosticIntakePolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        "local-extract" => Some(local_extract()),
        _ => None,
    }
}

/// Deserialize the inbound `DIAGNOSTIC_INTAKE` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<DiagnosticIntakeEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid DIAGNOSTIC_INTAKE event: {err}")))
}

/// Read `diagnostic_intake.policy` (a [`PartialDiagnosticIntakePolicy`]) out
/// of the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D), parameterized
/// by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialDiagnosticIntakePolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialDiagnosticIntakePolicy`] bundle, preferring a `harness.json`
/// `diagnostic_intake.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries no
/// `profile` field, and `Err` when a `profile` name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialDiagnosticIntakePolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `diagnostic_intake.policy` section out of
/// a worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already had a
/// worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialDiagnosticIntakePolicy>, NodeError> {
    read_harness_policy_defaults_from(&PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

/// Resolve `event.profile` against a worktree. Thin wrapper over
/// [`resolve_profile_from`] with [`PolicyConfigSource::Worktree`], kept so
/// tests/callers that already had a worktree path in hand don't need to
/// construct a source.
#[cfg(test)]
fn resolve_profile(
    profile_name: Option<&str>,
    worktree: &Path,
) -> Result<Option<PartialDiagnosticIntakePolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`DiagnosticIntakePolicy`] for this run against an
/// arbitrary [`PolicyConfigSource`]: the inbound event's `policy` override,
/// the resolved `profile` bundle, `source`'s `diagnostic_intake.policy`
/// defaults, and the built-in default, high->low precedence via
/// [`policy::resolve`]. A [`PolicyConfigSource::Builtin`] source resolves
/// successfully with no filesystem access — the case a worktree-free
/// (channel/API-triggered) run needs.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<DiagnosticIntakePolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    Ok(policy::resolve(
        DiagnosticIntakePolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

/// Resolve the four-layer [`DiagnosticIntakePolicy`] for this run: the
/// inbound event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `diagnostic_intake.policy` defaults,
/// and the built-in default, high->low precedence via [`policy::resolve`].
/// This is what `IntakeExtractNode::process` calls — there is no dedicated
/// setup node in this workflow. Thin wrapper over
/// [`resolve_policy_for_run_from`] with [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<DiagnosticIntakePolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-diagnostic-intake-profiles-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn base_event() -> DiagnosticIntakeEventSchema {
        DiagnosticIntakeEventSchema {
            notes: "Client call transcript: ...".to_string(),
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn base_ctx(event: DiagnosticIntakeEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn profile_by_name_resolves_all_four_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("cheap-fast"), Some(cheap_fast()));
        assert_eq!(profile_by_name("thorough"), Some(thorough()));
        assert_eq!(profile_by_name("local-extract"), Some(local_extract()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.extract, Some(ModelTier::Sonnet));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Normal));
        assert_eq!(p.prompt_cache, Some(false));
    }

    #[test]
    fn cheap_fast_matches_documented_knob_values() {
        let p = cheap_fast();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.extract, Some(ModelTier::Haiku));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(p.prompt_cache, Some(true));
    }

    #[test]
    fn thorough_matches_documented_knob_values() {
        let p = thorough();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.extract, Some(ModelTier::Opus));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Verbose));
    }

    #[test]
    fn local_extract_resolves_extract_tier_to_local_with_constrained_json() {
        let p = local_extract();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.extract, Some(ModelTier::Local));
        let local = p.local.expect("local config set");
        assert_eq!(local.constrained_json, Some(true));
    }

    #[test]
    fn local_extract_profile_resolves_to_local_tier_end_to_end() {
        let resolved = policy::resolve(
            DiagnosticIntakePolicy::default(),
            None,
            Some(&local_extract()),
            None,
        );
        assert_eq!(resolved.model_tiers.extract, ModelTier::Local);
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn harness_json_diagnostic_intake_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "diagnostic_intake": {
                    "policy": {
                        "output_verbosity": "terse",
                        "model_tiers": { "extract": "haiku" }
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "model_tiers": { "extract": "sonnet" }
                        }
                    }
                }
            })
            .to_string(),
        )
        .unwrap();

        let defaults = read_harness_policy_defaults(&worktree)
            .expect("should succeed")
            .expect("policy section present");
        assert_eq!(defaults.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(
            defaults.model_tiers.unwrap().extract,
            Some(ModelTier::Haiku)
        );

        let resolved = resolve_profile(Some("baseline"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(
            resolved.model_tiers.unwrap().extract,
            Some(ModelTier::Sonnet)
        );
    }

    #[test]
    fn read_harness_policy_defaults_returns_none_when_file_missing() {
        let worktree = temp_dir();
        let result = read_harness_policy_defaults(&worktree).expect("should succeed");
        assert!(result.is_none());
    }

    #[test]
    fn resolve_profile_falls_back_to_builtin_when_no_harness_entry() {
        let worktree = temp_dir();
        let result = resolve_profile(Some("cheap-fast"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result, cheap_fast());
    }

    #[test]
    fn resolve_profile_unknown_name_errors() {
        let worktree = temp_dir();
        let err = resolve_profile(Some("nonexistent"), &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_with_no_overrides_returns_builtin_default() {
        let worktree = temp_dir();
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, DiagnosticIntakePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_local_extract_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("local-extract".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.extract, ModelTier::Local);
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "diagnostic_intake": {
                    "policy": { "model_tiers": { "extract": "haiku" } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = base_event();
        event.policy = Some(PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Opus),
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.extract, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_unknown_profile_name_errors() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("nonexistent".to_string());
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = base_ctx(base_event());
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, DiagnosticIntakePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_still_errors_on_unknown_profile() {
        let mut event = base_event();
        event.profile = Some("nonexistent".to_string());
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_from_harness_file_source_preserves_precedence() {
        let dir = temp_dir();
        let harness_file = dir.join("standalone-harness.json");
        std::fs::write(
            &harness_file,
            serde_json::json!({
                "diagnostic_intake": { "policy": { "model_tiers": { "extract": "haiku" } } }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let mut event = base_event();
        event.policy = Some(PartialDiagnosticIntakePolicy {
            model_tiers: Some(PartialModelTiers {
                extract: Some(ModelTier::Opus),
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.model_tiers.extract, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("local-extract".to_string());
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }
}
