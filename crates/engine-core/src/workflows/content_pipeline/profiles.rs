//! Named `ContentPipelinePolicy` profile bundles, the
//! `content_pipeline.{policy,profiles}` `harness.json` section reader, and
//! `resolve_policy_for_run`.
//!
//! Mirrors `proposal_generator::profiles` (the named-bundle catalog +
//! `read_harness_policy_defaults`/`resolve_profile`/`resolve_policy_for_run`
//! trio), generalized over the shared `crate::policy::profiles` plumbing
//! (EN.4.0). There is no setup node in this workflow — `SourceRouterNode`
//! calls [`resolve_policy_for_run_from`] directly, since a channel envelope
//! has no repo checkout to derive a worktree from
//! ([`crate::policy::PolicyConfigSource`] decouples resolution from a
//! worktree path).
//!
//! Caller-supplied loop bounds (`max_critic_iterations`,
//! `critic_confidence_threshold`) are validated on the *final* resolved
//! policy — after all four layers have merged — via
//! `policy::validate_bounds`, so an out-of-range value from any layer
//! (harness defaults, a named profile, or a per-event override) surfaces as
//! a rejected run rather than a silently accepted or clamped one.

use std::path::Path;

use engine_contract::TaskContext;

use crate::node::NodeError;
use crate::policy::PolicyConfigSource;

use super::policy::{
    self, validate_bounds, ContentPipelinePolicy, HarvestMode, ModelTier, OutputVerbosity,
    PartialContentPipelinePolicy, PartialHarvestConfig, PartialLocalConfig,
    PartialMaterializeConfig, PartialModelTiers,
};
use super::schema::ContentPipelineInput;

/// The `harness.json` section key this workflow's policy/profiles live
/// under (`content_pipeline.policy` / `content_pipeline.profiles`), passed
/// to the generic `crate::policy::profiles` plumbing.
const WORKFLOW_KEY: &str = "content_pipeline";

/// The explicit control profile: all-Sonnet, normal verbosity, prompt cache
/// off, default loop bounds (3 iterations, 0.8 confidence threshold).
/// Spelled out explicitly (rather than left all-`None`) so selecting
/// `profile: "baseline"` is a legible, self-documenting no-op against the
/// built-in default.
#[must_use]
pub fn baseline() -> PartialContentPipelinePolicy {
    PartialContentPipelinePolicy {
        output_verbosity: Some(OutputVerbosity::Normal),
        prompt_cache: Some(false),
        model_tiers: Some(PartialModelTiers {
            summarize: Some(ModelTier::Sonnet),
            critic: Some(ModelTier::Sonnet),
            revise: Some(ModelTier::Sonnet),
            translate: Some(ModelTier::Sonnet),
        }),
        local: None,
        max_critic_iterations: Some(3),
        critic_confidence_threshold: Some(0.8),
        dispatch_verbosity: Some(OutputVerbosity::Normal),
        // Restates the built-in `MaterializeConfig::default()` explicitly,
        // so `profile: "baseline"` reads as a legible no-op for the
        // materialize knob too (EN.7.D).
        materialize: Some(PartialMaterializeConfig {
            enabled: Some(true),
            corpus_root: Some(None),
            write: Some(true),
        }),
        // EN.7.C: restates the built-in `HarvestConfig::default()`
        // (`off`) explicitly — a legible no-op that relies on the existing
        // manifest/`index_brain` freshness reindex rather than an explicit
        // Synapse ingest POST.
        harvest: Some(PartialHarvestConfig {
            mode: Some(HarvestMode::Off),
        }),
    }
}

/// Cheapest/fastest drafting profile: all four Local-eligible stages
/// (`{summarize, critic, revise, translate}`) rewire to `ModelTier::Local`
/// with constrained JSON on.
#[must_use]
pub fn local_drafting() -> PartialContentPipelinePolicy {
    PartialContentPipelinePolicy {
        output_verbosity: None,
        prompt_cache: None,
        model_tiers: Some(PartialModelTiers {
            summarize: Some(ModelTier::Local),
            critic: Some(ModelTier::Local),
            revise: Some(ModelTier::Local),
            translate: Some(ModelTier::Local),
        }),
        local: Some(PartialLocalConfig {
            endpoint: None,
            model: None,
            constrained_json: Some(true),
        }),
        max_critic_iterations: None,
        critic_confidence_threshold: None,
        dispatch_verbosity: None,
        // Local drafting trades model tier, never durability: the digest is
        // still materialized to the corpus. `write` is stated rather than
        // inherited so the local path's on-disk effect is explicit and a
        // future harness-level `write: false` cannot silently reach it
        // (EN.7.D).
        materialize: Some(PartialMaterializeConfig {
            enabled: Some(true),
            corpus_root: None,
            write: Some(true),
        }),
        // EN.7.C: drafting locally is a model-tier trade, not an indexing
        // one — the freshness reindex still picks the written doc up, so
        // this stays off, same as baseline.
        harvest: Some(PartialHarvestConfig {
            mode: Some(HarvestMode::Off),
        }),
    }
}

/// Fast-summarize profile: `summarize` rewires to `ModelTier::Haiku`; every
/// other knob untouched (falls through to whatever layer resolves it).
#[must_use]
pub fn fast_summarize() -> PartialContentPipelinePolicy {
    PartialContentPipelinePolicy {
        output_verbosity: None,
        prompt_cache: None,
        model_tiers: Some(PartialModelTiers {
            summarize: Some(ModelTier::Haiku),
            critic: None,
            revise: None,
            translate: None,
        }),
        local: None,
        max_critic_iterations: None,
        critic_confidence_threshold: None,
        dispatch_verbosity: None,
        // Same reasoning as `local_drafting`: a cheaper summarize tier is a
        // quality trade, not a durability one — the digest is still written
        // (EN.7.D).
        materialize: Some(PartialMaterializeConfig {
            enabled: Some(true),
            corpus_root: None,
            write: Some(true),
        }),
        // EN.7.C: a cheaper summarize tier is a quality trade, not a
        // curation one — no extra network hop on the run's critical path.
        harvest: Some(PartialHarvestConfig {
            mode: Some(HarvestMode::Off),
        }),
    }
}

/// Curated-harvest profile: the one built-in bundle that opts into an
/// explicit, in-process Synapse ingest POST (`HarvestMode::InProcess`)
/// rather than relying on the existing manifest/`index_brain` freshness
/// reindex — for deployments that want instant/curated indexing right
/// after materialization. Every other knob is untouched (falls through to
/// whatever layer resolves it) so this profile composes with any model-tier
/// choice (EN.7.C).
#[must_use]
pub fn curated_harvest() -> PartialContentPipelinePolicy {
    PartialContentPipelinePolicy {
        output_verbosity: None,
        prompt_cache: None,
        model_tiers: None,
        local: None,
        max_critic_iterations: None,
        critic_confidence_threshold: None,
        dispatch_verbosity: None,
        materialize: None,
        harvest: Some(PartialHarvestConfig {
            mode: Some(HarvestMode::InProcess),
        }),
    }
}

/// Resolve a built-in profile bundle by its kebab-case name. Returns `None`
/// for any name that isn't one of the four canonical profiles.
#[must_use]
pub fn profile_by_name(name: &str) -> Option<PartialContentPipelinePolicy> {
    match name {
        "baseline" => Some(baseline()),
        "local-drafting" => Some(local_drafting()),
        "fast-summarize" => Some(fast_summarize()),
        "curated-harvest" => Some(curated_harvest()),
        _ => None,
    }
}

/// Deserialize the inbound `CONTENT_PIPELINE` event from `ctx.event`.
fn parse_event(ctx: &TaskContext) -> Result<ContentPipelineInput, NodeError> {
    serde_json::from_value(ctx.event.clone())
        .map_err(|err| NodeError::new(format!("invalid CONTENT_PIPELINE event: {err}")))
}

/// Read `content_pipeline.policy` (a [`PartialContentPipelinePolicy`]) out
/// of the file addressed by `source`. Delegates to the generic
/// `crate::policy::read_harness_policy_defaults_from` (EN.5.D),
/// parameterized by [`WORKFLOW_KEY`].
fn read_harness_policy_defaults_from(
    source: &PolicyConfigSource,
) -> Result<Option<PartialContentPipelinePolicy>, NodeError> {
    crate::policy::read_harness_policy_defaults_from(source, WORKFLOW_KEY)
}

/// Resolve `event.profile` (a named profile) to a
/// [`PartialContentPipelinePolicy`] bundle, preferring a `harness.json`
/// `content_pipeline.profiles[name]` entry (read via `source`) over the
/// built-in [`profile_by_name`]. Returns `Ok(None)` when the event carries no
/// `profile` field, and `Err` when a `profile` name is given but resolves to
/// neither source (no silent no-op). Delegates to the generic
/// `crate::policy::resolve_profile_from` (EN.5.D), parameterized by
/// [`WORKFLOW_KEY`] and [`profile_by_name`].
fn resolve_profile_from(
    profile_name: Option<&str>,
    source: &PolicyConfigSource,
) -> Result<Option<PartialContentPipelinePolicy>, NodeError> {
    crate::policy::resolve_profile_from(profile_name, source, WORKFLOW_KEY, profile_by_name)
}

/// Read `planning/harness.json`'s `content_pipeline.policy` section out of
/// a worktree. Thin wrapper over [`read_harness_policy_defaults_from`] with
/// [`PolicyConfigSource::Worktree`], kept so tests/callers that already had
/// a worktree path in hand don't need to construct a source.
#[cfg(test)]
fn read_harness_policy_defaults(
    worktree: &Path,
) -> Result<Option<PartialContentPipelinePolicy>, NodeError> {
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
) -> Result<Option<PartialContentPipelinePolicy>, NodeError> {
    resolve_profile_from(
        profile_name,
        &PolicyConfigSource::Worktree(worktree.to_path_buf()),
    )
}

/// Resolve the four-layer [`ContentPipelinePolicy`] for this run against an
/// arbitrary [`PolicyConfigSource`]: the inbound event's `policy` override,
/// the resolved `profile` bundle, `source`'s `content_pipeline.policy`
/// defaults, and the built-in default, high->low precedence via
/// [`policy::resolve`]. A [`PolicyConfigSource::Builtin`] source resolves
/// successfully with no filesystem access — the case a worktree-free
/// (channel/API-triggered) run needs.
///
/// The resolved policy's loop bounds are then validated via
/// [`validate_bounds`]: an out-of-range `max_critic_iterations` or
/// `critic_confidence_threshold` (from any layer) surfaces as `Err` rather
/// than being silently clamped or accepted.
pub fn resolve_policy_for_run_from(
    ctx: &TaskContext,
    source: &PolicyConfigSource,
) -> Result<ContentPipelinePolicy, NodeError> {
    let event = parse_event(ctx)?;
    let harness_defaults = read_harness_policy_defaults_from(source)?;
    let profile = resolve_profile_from(event.profile.as_deref(), source)?;
    let resolved = policy::resolve(
        ContentPipelinePolicy::default(),
        harness_defaults.as_ref(),
        profile.as_ref(),
        event.policy.as_ref(),
    );
    validate_bounds(&resolved).map_err(NodeError::new)?;
    Ok(resolved)
}

/// Resolve the four-layer [`ContentPipelinePolicy`] for this run: the
/// inbound event's `policy` override, the resolved `profile` bundle, the
/// worktree's `planning/harness.json` `content_pipeline.policy` defaults,
/// and the built-in default, high->low precedence via [`policy::resolve`].
/// Thin wrapper over [`resolve_policy_for_run_from`] with
/// [`PolicyConfigSource::Worktree`].
pub fn resolve_policy_for_run(
    ctx: &TaskContext,
    worktree: &Path,
) -> Result<ContentPipelinePolicy, NodeError> {
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
            "engine-core-content-pipeline-profiles-test-{}-{n}",
            std::process::id()
        ));
        // Guarantee-empty: see `sdlc_flow/setup.rs`'s `temp_dir_named` doc
        // comment for why PID-recycling makes this removal necessary, not
        // optional.
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn minimal_web_article_event() -> serde_json::Value {
        serde_json::json!({
            "envelope": {
                "envelope_id": "env-1",
                "channel_type": "web_article",
                "timestamp": "2026-07-25T00:00:00Z",
                "source": { "kind": "url", "url": "https://example.com/a" }
            }
        })
    }

    fn base_ctx(event: serde_json::Value) -> TaskContext {
        TaskContext {
            event,
            nodes: HashMap::new(),
            metadata: serde_json::json!({}),
            node_runs: HashMap::new(),
        }
    }

    #[test]
    fn profile_by_name_resolves_all_four_canonical_names() {
        assert_eq!(profile_by_name("baseline"), Some(baseline()));
        assert_eq!(profile_by_name("local-drafting"), Some(local_drafting()));
        assert_eq!(profile_by_name("fast-summarize"), Some(fast_summarize()));
        assert_eq!(profile_by_name("curated-harvest"), Some(curated_harvest()));
    }

    #[test]
    fn profile_by_name_returns_none_for_unknown_name() {
        assert_eq!(profile_by_name("nonexistent"), None);
    }

    #[test]
    fn baseline_matches_documented_knob_values() {
        let p = baseline();
        let tiers = p.model_tiers.expect("model_tiers set");
        assert_eq!(tiers.summarize, Some(ModelTier::Sonnet));
        assert_eq!(tiers.critic, Some(ModelTier::Sonnet));
        assert_eq!(tiers.revise, Some(ModelTier::Sonnet));
        assert_eq!(tiers.translate, Some(ModelTier::Sonnet));
        assert_eq!(p.output_verbosity, Some(OutputVerbosity::Normal));
        assert_eq!(p.prompt_cache, Some(false));
        assert_eq!(p.max_critic_iterations, Some(3));
        assert_eq!(p.critic_confidence_threshold, Some(0.8));
        assert_eq!(p.dispatch_verbosity, Some(OutputVerbosity::Normal));
    }

    #[test]
    fn local_drafting_resolves_all_four_stages_to_local() {
        let resolved = policy::resolve(
            ContentPipelinePolicy::default(),
            None,
            Some(&local_drafting()),
            None,
        );
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Local);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Local);
        assert!(resolved.local.constrained_json);
    }

    #[test]
    fn fast_summarize_resolves_summarize_to_haiku_only() {
        let resolved = policy::resolve(
            ContentPipelinePolicy::default(),
            None,
            Some(&fast_summarize()),
            None,
        );
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Haiku);
        // Other stages untouched by this profile.
        assert_eq!(resolved.model_tiers.critic, ModelTier::Sonnet);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Sonnet);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Sonnet);
    }

    #[test]
    fn harness_json_content_pipeline_section_parses_into_partial_types() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "content_pipeline": {
                    "policy": {
                        "output_verbosity": "terse",
                        "model_tiers": { "summarize": "haiku" }
                    },
                    "profiles": {
                        "_comment": "not a bundle",
                        "baseline": {
                            "model_tiers": {
                                "summarize": "sonnet",
                                "critic": "sonnet",
                                "revise": "sonnet",
                                "translate": "sonnet"
                            }
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
            defaults.model_tiers.unwrap().summarize,
            Some(ModelTier::Haiku)
        );

        let resolved = resolve_profile(Some("baseline"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(
            resolved.model_tiers.unwrap().summarize,
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
        let result = resolve_profile(Some("local-drafting"), &worktree)
            .expect("should succeed")
            .expect("profile resolved");
        assert_eq!(result, local_drafting());
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
        let ctx = base_ctx(minimal_web_article_event());
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved, ContentPipelinePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_applies_named_local_drafting_profile() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("local-drafting");
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Local);
        assert_eq!(resolved.model_tiers.critic, ModelTier::Local);
        assert_eq!(resolved.model_tiers.revise, ModelTier::Local);
        assert_eq!(resolved.model_tiers.translate, ModelTier::Local);
    }

    #[test]
    fn resolve_policy_for_run_applies_named_fast_summarize_profile() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("fast-summarize");
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.summarize, ModelTier::Haiku);
    }

    #[test]
    fn resolve_policy_for_run_event_override_beats_harness_defaults() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "content_pipeline": {
                    "policy": { "model_tiers": { "critic": "haiku" } }
                }
            })
            .to_string(),
        )
        .unwrap();

        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({
            "model_tiers": { "critic": "opus" }
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run(&ctx, &worktree).expect("resolve should succeed");
        assert_eq!(resolved.model_tiers.critic, ModelTier::Opus);
    }

    #[test]
    fn resolve_policy_for_run_unknown_profile_name_errors() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("nonexistent");
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree).expect_err("should fail");
        assert!(err.message.contains("unknown profile"));
    }

    #[test]
    fn resolve_policy_for_run_rejects_out_of_range_max_critic_iterations_override() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({ "max_critic_iterations": 999 });
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree)
            .expect_err("out-of-range max_critic_iterations should be rejected");
        assert!(err.message.contains("max_critic_iterations"));
    }

    #[test]
    fn resolve_policy_for_run_rejects_out_of_range_confidence_threshold_override() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({ "critic_confidence_threshold": 1.5 });
        let ctx = base_ctx(event);
        let err = resolve_policy_for_run(&ctx, &worktree)
            .expect_err("out-of-range critic_confidence_threshold should be rejected");
        assert!(err.message.contains("critic_confidence_threshold"));
    }

    // --- resolve_policy_for_run_from / PolicyConfigSource --------------------

    #[test]
    fn resolve_policy_for_run_from_builtin_source_needs_no_worktree() {
        let ctx = base_ctx(minimal_web_article_event());
        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed with no filesystem access");
        assert_eq!(resolved, ContentPipelinePolicy::default());
    }

    #[test]
    fn resolve_policy_for_run_from_builtin_source_still_errors_on_unknown_profile() {
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("nonexistent");
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
                "content_pipeline": { "policy": { "model_tiers": { "critic": "haiku" } } }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::HarnessFile(harness_file);

        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({
            "model_tiers": { "critic": "opus" }
        });
        let ctx = base_ctx(event);
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        // event > harness default
        assert_eq!(resolved.model_tiers.critic, ModelTier::Opus);
    }

    // EN.7.D task 5 — the `materialize` knob across the named profiles.

    #[test]
    fn every_named_profile_sets_materialize_explicitly() {
        for (name, bundle) in [
            ("baseline", baseline()),
            ("local-drafting", local_drafting()),
            ("fast-summarize", fast_summarize()),
        ] {
            let materialize = bundle
                .materialize
                .unwrap_or_else(|| panic!("profile '{name}' must state the materialize knob"));
            assert_eq!(
                materialize.enabled,
                Some(true),
                "profile '{name}' should keep materialization on"
            );
            assert_eq!(
                materialize.write,
                Some(true),
                "profile '{name}' should state its on-disk write behavior"
            );
        }
    }

    // EN.7.C task 3 — the `harvest` knob across the named profiles.

    #[test]
    fn every_named_profile_sets_harvest_explicitly() {
        for (name, bundle) in [
            ("baseline", baseline()),
            ("local-drafting", local_drafting()),
            ("fast-summarize", fast_summarize()),
            ("curated-harvest", curated_harvest()),
        ] {
            let harvest = bundle
                .harvest
                .unwrap_or_else(|| panic!("profile '{name}' must state the harvest knob"));
            assert!(
                harvest.mode.is_some(),
                "profile '{name}' must state an explicit harvest mode"
            );
        }
    }

    #[test]
    fn baseline_local_drafting_and_fast_summarize_resolve_harvest_off() {
        for bundle in [baseline(), local_drafting(), fast_summarize()] {
            let resolved =
                policy::resolve(ContentPipelinePolicy::default(), None, Some(&bundle), None);
            assert_eq!(resolved.harvest.mode, HarvestMode::Off);
        }
    }

    #[test]
    fn curated_harvest_profile_resolves_harvest_in_process() {
        let resolved = policy::resolve(
            ContentPipelinePolicy::default(),
            None,
            Some(&curated_harvest()),
            None,
        );
        assert_eq!(resolved.harvest.mode, HarvestMode::InProcess);
    }

    #[test]
    fn baseline_profile_materialize_resolves_to_the_builtin_default() {
        let resolved = policy::resolve(
            ContentPipelinePolicy::default(),
            None,
            Some(&baseline()),
            None,
        );

        assert_eq!(
            resolved.materialize,
            ContentPipelinePolicy::default().materialize,
            "baseline must be an explicit no-op against the built-in default"
        );
    }

    #[test]
    fn event_materialize_override_beats_the_named_profile_per_field() {
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("local-drafting");
        event["policy"] = serde_json::json!({
            "materialize": { "enabled": false }
        });
        let ctx = base_ctx(event);

        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");

        // The event wins on `enabled` ...
        assert!(!resolved.materialize.enabled);
        // ... while the untouched fields still come from the profile.
        assert!(resolved.materialize.write);
        assert_eq!(resolved.materialize.corpus_root, None);
    }

    #[test]
    fn event_materialize_corpus_root_override_reaches_the_resolved_policy() {
        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({
            "materialize": { "corpus_root": "/tmp/some-corpus" }
        });
        let ctx = base_ctx(event);

        let resolved = resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Builtin)
            .expect("resolve should succeed");

        assert_eq!(
            resolved.materialize.corpus_root.as_deref(),
            Some("/tmp/some-corpus")
        );
        assert!(resolved.materialize.enabled);
    }

    #[test]
    fn harness_materialize_defaults_beat_the_builtin_and_lose_to_the_event() {
        let worktree = temp_dir();
        std::fs::create_dir_all(worktree.join("planning")).unwrap();
        std::fs::write(
            worktree.join("planning").join("harness.json"),
            serde_json::json!({
                "content_pipeline": {
                    "policy": { "materialize": { "enabled": false, "write": false } }
                }
            })
            .to_string(),
        )
        .unwrap();
        let source = PolicyConfigSource::Worktree(worktree);

        // Harness beats the built-in default ...
        let ctx = base_ctx(minimal_web_article_event());
        let resolved = resolve_policy_for_run_from(&ctx, &source).expect("resolve should succeed");
        assert!(!resolved.materialize.enabled);
        assert!(!resolved.materialize.write);

        // ... and loses to the per-run event override.
        let mut event = minimal_web_article_event();
        event["policy"] = serde_json::json!({ "materialize": { "enabled": true } });
        let resolved =
            resolve_policy_for_run_from(&base_ctx(event), &source).expect("resolve should succeed");
        assert!(resolved.materialize.enabled);
        assert!(
            !resolved.materialize.write,
            "write still comes from harness"
        );
    }

    #[test]
    fn resolve_policy_for_run_wrapper_matches_from_worktree_source() {
        let worktree = temp_dir();
        let mut event = minimal_web_article_event();
        event["profile"] = serde_json::json!("local-drafting");
        let ctx = base_ctx(event);

        let via_wrapper = resolve_policy_for_run(&ctx, &worktree).expect("should succeed");
        let via_from =
            resolve_policy_for_run_from(&ctx, &PolicyConfigSource::Worktree(worktree.clone()))
                .expect("should succeed");
        assert_eq!(via_wrapper, via_from);
    }
}
