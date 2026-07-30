//! Named `ResearchAgentPolicy` profile bundles, the
//! `research_agent.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run` — filled in task 4.
//!
//! Mirrors `sdlc_flow::profiles` (the named-bundle catalog) +
//! `sdlc_flow::setup`'s `read_harness_policy_defaults`/`resolve_profile`/
//! `resolve_policy_for_run` trio, generalized over the shared
//! `crate::policy::profiles` plumbing (EN.4.0). There is no setup node in
//! this workflow — the two terminal nodes (`CompanyResearchNode`,
//! `ProspectingResearchNode`) call [`resolve_policy_for_run`] directly.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{
    self, ContactDepth, ModelTier, OutputVerbosity, PartialContactEnrichment,
    PartialIngressDispatch, PartialModelTiers, PartialResearchAgentPolicy, ResearchAgentPolicy,
};
use super::schema::ResearchAgentEventSchema;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`research_agent.policy` / `research_agent.profiles`), passed to
/// the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "research_agent";

/// The explicit control profile: Sonnet on both stages, normal verbosity,
/// prompt cache off, ingress dispatch off. Spelled out explicitly (rather
/// than left all-`None`) so selecting `profile: "baseline"` is a legible,
/// self-documenting no-op against the built-in default.
#[must_use]
pub fn baseline() -> PartialResearchAgentPolicy {
    PartialResearchAgentPolicy {
        output_verbosity: Some(OutputVerbosity::Normal),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Sonnet),
            prospect: Some(ModelTier::Sonnet),
        }),
        contact_enrichment: Some(PartialContactEnrichment {
            research: Some(ContactDepth::Standard),
            prospect: Some(ContactDepth::Standard),
            max_fetches: Some(4),
        }),
        ingress_dispatch: Some(PartialIngressDispatch {
            enabled: Some(false),
            target_workflow_type: Some("CONTENT_PIPELINE".to_string()),
        }),
        ..Default::default()
    }
}

/// Cheapest/fastest profile: `haiku` on both stages, terse output, prompt
/// caching on, ingress dispatch off (a chained `CONTENT_PIPELINE` run is
/// the single largest cost a research run can incur).
#[must_use]
pub fn cheap_fast() -> PartialResearchAgentPolicy {
    PartialResearchAgentPolicy {
        output_verbosity: Some(OutputVerbosity::Terse),
        prompt_cache: Some(true),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Haiku),
            prospect: Some(ModelTier::Haiku),
        }),
        contact_enrichment: Some(PartialContactEnrichment {
            research: Some(ContactDepth::Off),
            prospect: Some(ContactDepth::Off),
            max_fetches: Some(0),
        }),
        ingress_dispatch: Some(PartialIngressDispatch {
            enabled: Some(false),
            target_workflow_type: Some("CONTENT_PIPELINE".to_string()),
        }),
        ..Default::default()
    }
}

/// Highest-quality profile: `opus` on both stages, verbose output, ingress
/// dispatch on (the quality ceiling *is* the closed loop into
/// `CONTENT_PIPELINE`).
#[must_use]
pub fn thorough() -> PartialResearchAgentPolicy {
    PartialResearchAgentPolicy {
        output_verbosity: Some(OutputVerbosity::Verbose),
        model_tiers: Some(PartialModelTiers {
            research: Some(ModelTier::Opus),
            prospect: Some(ModelTier::Opus),
        }),
        // Prospecting deliberately stays `standard` even at `thorough` so a
        // broad sweep never multiplies deep enrichment across dozens of
        // leads.
        contact_enrichment: Some(PartialContactEnrichment {
            research: Some(ContactDepth::Deep),
            prospect: Some(ContactDepth::Standard),
            max_fetches: Some(8),
        }),
        ingress_dispatch: Some(PartialIngressDispatch {
            enabled: Some(true),
            target_workflow_type: Some("CONTENT_PIPELINE".to_string()),
        }),
        ..Default::default()
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the three canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialResearchAgentPolicy> {
    match name {
        "baseline" => Some(baseline()),
        "cheap-fast" => Some(cheap_fast()),
        "thorough" => Some(thorough()),
        _ => None,
    }
}

/// Deserialize the inbound `RESEARCH_AGENT` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ResearchAgentEventSchema, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid RESEARCH_AGENT event: {err}")))
}

/// Read `research_agent.policy` (a [`PartialResearchAgentPolicy`]) out of
/// the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D), parameterized
/// by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialResearchAgentPolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialResearchAgentPolicy`] bundle, preferring a `harness.json`
/// `research_agent.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries no
/// `profile` field, and `Err` when a `profile` name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialResearchAgentPolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `research_agent.policy` section out of a
/// worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already had a
/// worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialResearchAgentPolicy>, NodeError> {
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
) -> Result<Option<PartialResearchAgentPolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`ResearchAgentPolicy`] for this run against an
/// arbitrary [`PolicyConfigSource`]: the inbound event's `policy` override,
/// the resolved `profile` bundle, `source`'s `research_agent.policy`
/// defaults, and the built-in default, high->low precedence via
/// [`policy::resolve`]. A [`PolicyConfigSource::Builtin`] source resolves
/// successfully with no filesystem access — the case a worktree-free
/// (channel/API-triggered) run needs.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<ResearchAgentPolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    Ok(policy::resolve(
        ResearchAgentPolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    ))
}

/// Resolve the four-layer [`ResearchAgentPolicy`] for this run: the inbound
/// event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `research_agent.policy` defaults, and
/// the built-in default, high->low precedence via [`policy::resolve`]. This
/// is what both terminal nodes (`CompanyResearchNode`,
/// `ProspectingResearchNode`) call — there is no dedicated setup node in
/// this workflow. Thin wrapper over [`resolve_policy_for_run_from`] with
/// [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<ResearchAgentPolicy, NodeError> {
    resolve_policy_for_run_from(ctx, &PolicyConfigSource::Worktree(worktree.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::schema::ResearchMode;
    use super::*;

    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "engine-core-research-agent-profiles-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn base_event() -> ResearchAgentEventSchema {
        ResearchAgentEventSchema {
            mode: ResearchMode::Company,
            company_name: Some("Acme Corp".to_string()),
            company_url: None,
            vertical: None,
            topic: None,
            locale: crate::locale::Locale::default(),
            policy: None,
            profile: None,
        }
    }

    fn base_ctx(event: ResearchAgentEventSchema) -> TaskContext {
        TaskContext {
            event: serde_json::to_value(event).unwrap(),
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
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

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.clone().expect("model_tiers set");
        assert_eq!(tiers.research, Some(ModelTier::Sonnet));
        assert_eq!(tiers.prospect, Some(ModelTier::Sonnet));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Normal));
        assert_eq!(p.prompt_cache, Some(false));
        let ce = p.contact_enrichment.expect("contact_enrichment set");
        assert_eq!(ce.research, Some(ContactDepth::Standard));
        assert_eq!(ce.prospect, Some(ContactDepth::Standard));
        assert_eq!(ce.max_fetches, Some(4));
    }

    #[test]
    fn cheap_fast_matches_documented_knob_values() {
        let p = cheap_fast();
        let tiers = p.model_tiers.clone().expect("model_tiers set");
        assert_eq!(tiers.research, Some(ModelTier::Haiku));
        assert_eq!(tiers.prospect, Some(ModelTier::Haiku));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Terse));
        assert_eq!(p.prompt_cache, Some(true));
        let ce = p.contact_enrichment.expect("contact_enrichment set");
        assert_eq!(ce.research, Some(ContactDepth::Off));
        assert_eq!(ce.prospect, Some(ContactDepth::Off));
        assert_eq!(ce.max_fetches, Some(0));
    }

    #[test]
    fn thorough_matches_documented_knob_values() {
        let p = thorough();
        let tiers = p.model_tiers.clone().expect("model_tiers set");
        assert_eq!(tiers.research, Some(ModelTier::Opus));
        assert_eq!(tiers.prospect, Some(ModelTier::Opus));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Verbose));
        let ce = p.contact_enrichment.expect("contact_enrichment set");
        assert_eq!(ce.research, Some(ContactDepth::Deep));
        assert_eq!(ce.prospect, Some(ContactDepth::Standard));
        assert_eq!(ce.max_fetches, Some(8));
    }

    #[test]
    fn baseline_sets_ingress_dispatch_disabled() {
        let p = baseline();
        let id = p.ingress_dispatch.expect("ingress_dispatch set");
        assert_eq!(id.enabled, Some(false));
        assert_eq!(
            id.target_workflow_type,
            Some("CONTENT_PIPELINE".to_string())
        );
    }

    #[test]
    fn cheap_fast_sets_ingress_dispatch_disabled() {
        let p = cheap_fast();
        let id = p.ingress_dispatch.expect("ingress_dispatch set");
        assert_eq!(id.enabled, Some(false));
        assert_eq!(
            id.target_workflow_type,
            Some("CONTENT_PIPELINE".to_string())
        );
    }

    #[test]
    fn thorough_sets_ingress_dispatch_enabled() {
        let p = thorough();
        let id = p.ingress_dispatch.expect("ingress_dispatch set");
        assert_eq!(id.enabled, Some(true));
        assert_eq!(
            id.target_workflow_type,
            Some("CONTENT_PIPELINE".to_string())
        );
    }

    #[test]
    fn resolve_policy_for_run_baseline_profile_resolves_ingress_dispatch_disabled() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("baseline".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert!(!resolved.ingress_dispatch.enabled);
    }

    #[test]
    fn resolve_policy_for_run_thorough_profile_resolves_ingress_dispatch_enabled() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("thorough".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert!(resolved.ingress_dispatch.enabled);
    }

    #[test]
    fn harness_json_research_agent_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "research_agent": {
                    "policy": {
                        "output_verbosity": "terse",
                        "model_tiers": { "research": "haiku" }
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "model_tiers": { "research": "sonnet", "prospect": "sonnet" }
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
            defaults.model_tiers.unwrap().research,
            Some(ModelTier::Haiku)
        );

        let resolved = resolve_profile(Some("baseline"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(
            resolved.model_tiers.unwrap().research,
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
        assert_eq!(resolved, ResearchAgentPolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_profile() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("cheap-fast".to_string());
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.research, ModelTier::Haiku);
        assert_eq!(resolved.output_verbosity, OutputVerbosity::Terse);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "research_agent": {
                    "policy": { "model_tiers": { "prospect": "haiku" } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = base_event();
        event.policy = Some(PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                prospect: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.prospect, ModelTier::Opus);
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
        assert_eq!(resolved, ResearchAgentPolicy::default());
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
                "research_agent": { "policy": { "model_tiers": { "prospect": "haiku" } } }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let mut event = base_event();
        event.policy = Some(PartialResearchAgentPolicy {
            model_tiers: Some(PartialModelTiers {
                prospect: Some(ModelTier::Opus),
                ..Default::default()
            }),
            ..Default::default()
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.model_tiers.prospect, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = base_event();
        event.profile = Some("cheap-fast".to_string());
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }
}
