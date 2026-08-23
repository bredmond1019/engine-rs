//! Integration coverage for the generic `engine-core::policy` framework
//! (EN.4.0 task 3), exercised against a small standalone test policy — not
//! `SdlcPolicy` — to prove the API is genuinely reusable by any workflow.
//!
//! Covers:
//! - `resolve<P>` four-layer precedence (`builtin < harness_defaults <
//!   profile < event_override`), including a field that only a deep layer
//!   overrides so the merge is proven field-by-field, not last-writer-wins
//!   wholesale.
//! - `ModelTier` -> model-string mapping for all four tiers.
//! - `RunTelemetry` harvest off a hand-built synthetic `TaskContext`
//!   (`node_runs` usage + `nodes` cost/verdict), asserting summed
//!   tokens/cost/verdicts.
//! - `aggregate` grouping N runs into the right per-policy rows with
//!   correct sums/averages/pass_rate.

use std::collections::{BTreeMap, HashMap};

use chrono::Utc;
use engine_contract::{NodeRun, NodeRunStatus, TaskContext, Usage};
use engine_core::policy::{
    aggregate, harvest_telemetry, model_tier_to_model_string, resolve, LocalConfig, ModelTier,
    OutputVerbosity, Policy, RunTelemetry, RunTelemetryInputs,
};
use serde::{Deserialize, Serialize};

/// A small standalone policy type — deliberately not `SdlcPolicy` — used to
/// prove `resolve<P>`/`aggregate` are generic over any `Policy` impl.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
struct SamplePolicy {
    tier: ModelTier,
    verbosity: OutputVerbosity,
    max_retries: u32,
}

#[derive(Debug, Clone, Default)]
struct PartialSamplePolicy {
    tier: Option<ModelTier>,
    verbosity: Option<OutputVerbosity>,
    max_retries: Option<u32>,
}

impl Policy for SamplePolicy {
    type Partial = PartialSamplePolicy;

    fn apply(self, over: &Self::Partial) -> Self {
        Self {
            tier: over.tier.unwrap_or(self.tier),
            verbosity: over.verbosity.unwrap_or(self.verbosity),
            max_retries: over.max_retries.unwrap_or(self.max_retries),
        }
    }
}

// ---------------------------------------------------------------------
// resolve<P> precedence
// ---------------------------------------------------------------------

#[test]
fn resolve_precedence_across_all_four_layers_with_a_deep_merge_field() {
    // Builtin default: Sonnet / Normal / 1 retry.
    let builtin = SamplePolicy::default();
    assert_eq!(builtin.tier, ModelTier::Sonnet);
    assert_eq!(builtin.verbosity, OutputVerbosity::Normal);
    assert_eq!(builtin.max_retries, 0);

    // harness_defaults overrides tier + max_retries, leaves verbosity untouched.
    let harness_defaults = PartialSamplePolicy {
        tier: Some(ModelTier::Haiku),
        verbosity: None,
        max_retries: Some(2),
    };
    // profile overrides only verbosity (the "deep merge field" that no
    // other layer touches, proving field-by-field merge rather than
    // whole-struct replacement).
    let profile = PartialSamplePolicy {
        tier: None,
        verbosity: Some(OutputVerbosity::Verbose),
        max_retries: None,
    };
    // event_override wins for tier only.
    let event_override = PartialSamplePolicy {
        tier: Some(ModelTier::Opus),
        verbosity: None,
        max_retries: None,
    };

    let resolved = resolve(
        builtin,
        Some(&harness_defaults),
        Some(&profile),
        Some(&event_override),
    );

    // event_override beats harness_defaults for `tier`.
    assert_eq!(resolved.tier, ModelTier::Opus);
    // profile's verbosity survives untouched by harness or event.
    assert_eq!(resolved.verbosity, OutputVerbosity::Verbose);
    // harness_defaults' max_retries survives untouched by profile or event.
    assert_eq!(resolved.max_retries, 2);
}

#[test]
fn resolve_with_no_overrides_returns_builtin_unchanged() {
    let builtin = SamplePolicy {
        tier: ModelTier::Local,
        verbosity: OutputVerbosity::Terse,
        max_retries: 5,
    };
    let resolved = resolve(builtin, None, None, None);
    assert_eq!(resolved, builtin);
}

#[test]
fn resolve_only_harness_defaults_layer_applies() {
    let builtin = SamplePolicy::default();
    let harness_defaults = PartialSamplePolicy {
        tier: Some(ModelTier::Haiku),
        verbosity: Some(OutputVerbosity::Verbose),
        max_retries: Some(3),
    };
    let resolved = resolve(builtin, Some(&harness_defaults), None, None);
    assert_eq!(resolved.tier, ModelTier::Haiku);
    assert_eq!(resolved.verbosity, OutputVerbosity::Verbose);
    assert_eq!(resolved.max_retries, 3);
}

// ---------------------------------------------------------------------
// ModelTier -> model string mapping
// ---------------------------------------------------------------------

#[test]
fn model_tier_to_model_string_maps_all_four_tiers() {
    let local = LocalConfig::default();
    assert_eq!(
        model_tier_to_model_string(ModelTier::Sonnet, &local.model),
        "claude-sonnet-4-5"
    );
    assert_eq!(
        model_tier_to_model_string(ModelTier::Haiku, &local.model),
        "claude-haiku-4-5"
    );
    assert_eq!(
        model_tier_to_model_string(ModelTier::Opus, &local.model),
        "claude-opus-4-8"
    );
    assert_eq!(
        model_tier_to_model_string(ModelTier::Local, &local.model),
        local.model
    );
}

// ---------------------------------------------------------------------
// RunTelemetry harvest on a synthetic TaskContext
// ---------------------------------------------------------------------

fn synthetic_ctx() -> TaskContext {
    let started = Utc::now() - chrono::Duration::seconds(42);

    let mut node_runs = HashMap::new();
    node_runs.insert(
        "SetupNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: Some(started),
            completed_at: None,
            error: None,
            input: None,
            usage: None,
        },
    );
    node_runs.insert(
        "ImplementNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: Some(Usage {
                input_tokens: Some(120),
                output_tokens: Some(80),
                model: "claude-sonnet-4-5".to_string(),
            }),
        },
    );
    node_runs.insert(
        "ReviewNode".to_string(),
        NodeRun {
            status: NodeRunStatus::Success,
            started_at: None,
            completed_at: None,
            error: None,
            input: None,
            usage: Some(Usage {
                input_tokens: Some(30),
                output_tokens: Some(10),
                model: "claude-sonnet-4-5".to_string(),
            }),
        },
    );

    let mut nodes = HashMap::new();
    nodes.insert(
        "ImplementNode".to_string(),
        serde_json::json!({ "cost_usd": 0.42 }),
    );
    nodes.insert(
        "ReviewNode".to_string(),
        serde_json::json!({ "cost_usd": 0.08, "verdict": "PASS" }),
    );

    TaskContext {
        event: serde_json::json!({}),
        nodes,
        metadata: serde_json::json!({}),
        node_runs,
    }
}

#[test]
fn harvest_sums_tokens_cost_and_verdicts_from_synthetic_ctx() {
    let ctx = synthetic_ctx();
    let now = Utc::now();

    let mut model_tier_used = BTreeMap::new();
    model_tier_used.insert("implement".to_string(), "sonnet".to_string());
    model_tier_used.insert("review".to_string(), "sonnet".to_string());

    let inputs = RunTelemetryInputs {
        start_node_identity: "SetupNode",
        verdict_stages: &["ReviewNode"],
        cost_bearing_stages: &["ImplementNode", "ReviewNode"],
        total_attempts: 3,
        total_retries: 1,
        tasks_passed: 2,
        tasks_failed: 1,
        model_tier_used: model_tier_used.clone(),
        model_stages: &[],
    };

    let telemetry: RunTelemetry = harvest_telemetry(&ctx, now, inputs);

    assert!(telemetry.wall_clock_secs >= 41.0 && telemetry.wall_clock_secs <= 43.0);
    assert_eq!(telemetry.total_attempts, 3);
    assert_eq!(telemetry.total_retries, 1);
    assert_eq!(telemetry.tasks_passed, 2);
    assert_eq!(telemetry.tasks_failed, 1);
    assert_eq!(
        telemetry.review_verdicts,
        vec!["ReviewNode:PASS".to_string()]
    );
    assert_eq!(telemetry.total_input_tokens, 150);
    assert_eq!(telemetry.total_output_tokens, 90);
    assert!((telemetry.total_cost_usd - 0.50).abs() < 1e-9);
    assert_eq!(telemetry.model_tier_used, model_tier_used);
}

// ---------------------------------------------------------------------
// aggregate grouping
// ---------------------------------------------------------------------

fn make_telemetry(
    cost: f64,
    wall_clock: f64,
    passed: u32,
    failed: u32,
    verdict: &str,
) -> RunTelemetry {
    RunTelemetry {
        wall_clock_secs: wall_clock,
        total_attempts: passed + failed,
        total_retries: 0,
        tasks_passed: passed,
        tasks_failed: failed,
        review_verdicts: vec![verdict.to_string()],
        total_input_tokens: 10,
        total_output_tokens: 5,
        total_cost_usd: cost,
        total_cache_read_tokens: 0,
        total_cache_creation_tokens: 0,
        model_tier_used: BTreeMap::new(),
    }
}

#[test]
fn aggregate_groups_n_runs_into_correct_per_policy_rows() {
    let policy_a = SamplePolicy {
        tier: ModelTier::Sonnet,
        verbosity: OutputVerbosity::Normal,
        max_retries: 1,
    };
    let policy_b = SamplePolicy {
        tier: ModelTier::Haiku,
        verbosity: OutputVerbosity::Terse,
        max_retries: 0,
    };

    let runs = vec![
        (policy_a, make_telemetry(1.0, 10.0, 1, 0, "Review:PASS")),
        (policy_a, make_telemetry(3.0, 20.0, 1, 0, "Review:PASS")),
        (policy_a, make_telemetry(2.0, 15.0, 0, 1, "Review:FAIL")),
        (policy_b, make_telemetry(0.5, 5.0, 1, 0, "Review:PASS")),
    ];

    let rows = aggregate(&runs);
    assert_eq!(rows.len(), 2);

    let row_a = rows.iter().find(|r| r.policy == policy_a).unwrap();
    assert_eq!(row_a.run_count, 3);
    assert!((row_a.total_cost_usd - 6.0).abs() < 1e-9);
    assert!((row_a.avg_cost_usd - 2.0).abs() < 1e-9);
    assert!((row_a.total_wall_clock_secs - 45.0).abs() < 1e-9);
    assert!((row_a.avg_wall_clock_secs - 15.0).abs() < 1e-9);
    assert_eq!(row_a.total_tasks_passed, 2);
    assert_eq!(row_a.total_tasks_failed, 1);
    assert!((row_a.pass_rate - (2.0 / 3.0)).abs() < 1e-9);
    assert_eq!(row_a.review_verdict_counts.get("Review:PASS"), Some(&2));
    assert_eq!(row_a.review_verdict_counts.get("Review:FAIL"), Some(&1));

    let row_b = rows.iter().find(|r| r.policy == policy_b).unwrap();
    assert_eq!(row_b.run_count, 1);
    assert!((row_b.total_cost_usd - 0.5).abs() < 1e-9);
    assert_eq!(row_b.pass_rate, 1.0);
}

#[test]
fn aggregate_of_empty_runs_yields_no_rows() {
    let rows: Vec<engine_core::policy::PolicyAggregate<SamplePolicy>> = aggregate(&[]);
    assert!(rows.is_empty());
}
