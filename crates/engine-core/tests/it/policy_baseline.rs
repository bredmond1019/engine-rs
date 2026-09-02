//! Golden-fixture coverage for EN.14.J task 1: captures each of the nine
//! workflow surfaces' `baseline` profile, resolved with no per-run event
//! override and no `harness.json` present (`PolicyConfigSource::Builtin`
//! equivalent — `harness_defaults: None`), as canonical pretty JSON.
//!
//! # Why this exists (read before touching `crates/engine-core/src/policy/`)
//!
//! EN.14.J extracts a shared policy core out of nine independently-carried
//! `policy.rs`/`profiles.rs` pairs and collapses six `ModelTiers`
//! definitions into one. The block's stated risk is SILENT DEFAULT DRIFT —
//! an extraction that accidentally changes what `baseline` resolves to on
//! some surface, with nothing failing loudly. The only instrument that can
//! catch that is a fixture captured from the tree BEFORE the extraction
//! and asserted byte-identical AFTER.
//!
//! **These nine fixture files may only be regenerated when a surface's
//! `baseline` default is INTENTIONALLY changed — never merely to make this
//! test pass.** A fixture regenerated after a behavioural change encodes
//! the new behaviour and asserts nothing; it must be regenerated only with
//! a corresponding call-out in the changing block's completion note. To
//! regenerate: run this test with `UPDATE_POLICY_BASELINE=1` set, e.g.
//! `UPDATE_POLICY_BASELINE=1 cargo nextest run -p engine-core policy_baseline`.
//!
//! Surfaces enumerated from the tree at EN.14.J task 1 time: `approve_and_run`,
//! `content_pipeline`, `deliverable_render`, `diagnostic_intake`,
//! `linkedin_post`, `proposal_generator`, `research_agent`, `sdlc_flow`,
//! `sdlc_task`.

use std::path::PathBuf;

use engine_core::policy::resolve as resolve_policy;

/// Each surface's `baseline` profile bundle applied on top of that
/// surface's built-in default, with no harness-defaults layer and no event
/// override — i.e. exactly the layers `PolicyConfigSource::Builtin` would
/// leave in play — serialized to canonical pretty JSON.
fn resolve_baselines() -> Vec<(&'static str, serde_json::Value)> {
    use engine_core::workflows::{
        approve_and_run, content_pipeline, deliverable_render, diagnostic_intake, linkedin_post,
        proposal_generator, research_agent, sdlc_flow, sdlc_task,
    };

    vec![
        (
            "approve_and_run",
            serde_json::to_value(resolve_policy(
                approve_and_run::policy::ApproveAndRunPolicy::default(),
                None,
                Some(&approve_and_run::profiles::baseline()),
                None,
            ))
            .expect("ApproveAndRunPolicy serializes"),
        ),
        (
            "content_pipeline",
            serde_json::to_value(resolve_policy(
                content_pipeline::policy::ContentPipelinePolicy::default(),
                None,
                Some(&content_pipeline::profiles::baseline()),
                None,
            ))
            .expect("ContentPipelinePolicy serializes"),
        ),
        (
            "deliverable_render",
            serde_json::to_value(resolve_policy(
                deliverable_render::policy::DeliverableRenderPolicy::default(),
                None,
                Some(&deliverable_render::profiles::baseline()),
                None,
            ))
            .expect("DeliverableRenderPolicy serializes"),
        ),
        (
            "diagnostic_intake",
            serde_json::to_value(resolve_policy(
                diagnostic_intake::policy::DiagnosticIntakePolicy::default(),
                None,
                Some(&diagnostic_intake::profiles::baseline()),
                None,
            ))
            .expect("DiagnosticIntakePolicy serializes"),
        ),
        (
            "linkedin_post",
            serde_json::to_value(resolve_policy(
                linkedin_post::policy::LinkedInPostPolicy::default(),
                None,
                Some(&linkedin_post::profiles::baseline()),
                None,
            ))
            .expect("LinkedInPostPolicy serializes"),
        ),
        (
            "proposal_generator",
            serde_json::to_value(resolve_policy(
                proposal_generator::policy::ProposalGeneratorPolicy::default(),
                None,
                Some(&proposal_generator::profiles::baseline()),
                None,
            ))
            .expect("ProposalGeneratorPolicy serializes"),
        ),
        (
            "research_agent",
            serde_json::to_value(resolve_policy(
                research_agent::policy::ResearchAgentPolicy::default(),
                None,
                Some(&research_agent::profiles::baseline()),
                None,
            ))
            .expect("ResearchAgentPolicy serializes"),
        ),
        (
            "sdlc_flow",
            serde_json::to_value(resolve_policy(
                sdlc_flow::policy::SdlcPolicy::default(),
                None,
                Some(&sdlc_flow::profiles::baseline()),
                None,
            ))
            .expect("SdlcPolicy serializes"),
        ),
        (
            "sdlc_task",
            serde_json::to_value(resolve_policy(
                sdlc_task::policy::SdlcTaskPolicy::default(),
                None,
                Some(&sdlc_task::profiles::baseline()),
                None,
            ))
            .expect("SdlcTaskPolicy serializes"),
        ),
    ]
}

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy-baseline")
}

fn fixture_path(surface: &str) -> PathBuf {
    fixture_dir().join(format!("{surface}.json"))
}

fn canonical_json(value: &serde_json::Value) -> String {
    let mut out = serde_json::to_string_pretty(value).expect("value serializes to pretty JSON");
    out.push('\n');
    out
}

/// Regenerates the nine committed fixtures from the CURRENT tree. Only run
/// this deliberately (`UPDATE_POLICY_BASELINE=1`), and only when a
/// surface's `baseline` default changed intentionally — see the module doc.
#[test]
fn baseline_fixtures_are_captured_or_verified() {
    let update_mode = std::env::var("UPDATE_POLICY_BASELINE").is_ok();
    let dir = fixture_dir();

    if update_mode {
        std::fs::create_dir_all(&dir).expect("fixture dir is creatable");
        for (surface, value) in resolve_baselines() {
            std::fs::write(fixture_path(surface), canonical_json(&value))
                .unwrap_or_else(|err| panic!("failed to write {surface} fixture: {err}"));
        }
        return;
    }

    let mut drifted: Vec<String> = Vec::new();
    for (surface, value) in resolve_baselines() {
        let path = fixture_path(surface);
        let expected = std::fs::read_to_string(&path).unwrap_or_else(|err| {
            panic!(
                "failed to read baseline fixture for {surface} at {}: {err} \
                 (run with UPDATE_POLICY_BASELINE=1 to generate it)",
                path.display()
            )
        });
        let actual = canonical_json(&value);
        if actual != expected {
            drifted.push(surface.to_string());
        }
    }

    assert!(
        drifted.is_empty(),
        "baseline profile drifted for surface(s): {}. \
         Re-run with UPDATE_POLICY_BASELINE=1 ONLY if the drift is an \
         intentional default change, and call it out in the block's \
         completion note — never to silence this gate.",
        drifted.join(", ")
    );
}

/// Every surface's baseline fixture file exists and is non-empty — a
/// direct check on the nine-file inventory this AC requires, independent
/// of whether the content happens to match.
#[test]
fn all_nine_surfaces_have_a_committed_baseline_fixture() {
    let surfaces = [
        "approve_and_run",
        "content_pipeline",
        "deliverable_render",
        "diagnostic_intake",
        "linkedin_post",
        "proposal_generator",
        "research_agent",
        "sdlc_flow",
        "sdlc_task",
    ];
    for surface in surfaces {
        let path = fixture_path(surface);
        let contents = std::fs::read_to_string(&path)
            .unwrap_or_else(|err| panic!("missing baseline fixture for {surface}: {err}"));
        assert!(
            !contents.trim().is_empty(),
            "baseline fixture for {surface} at {} is empty",
            path.display()
        );
    }
}
