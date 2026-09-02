//! Parametrised nine-surface x three-profile resolution matrix for
//! EN.14.J task 4.
//!
//! This test asserts that resolving each of the three named profiles
//! (`baseline`, `cheap-fast`, `thorough`) SUCCEEDS on every one of the
//! nine workflow surfaces — it does not assert any *specific* resolved
//! value. Specific values are `policy_baseline`'s job (the golden-fixture
//! byte-identity gate from task 1); duplicating them here would make both
//! tests brittle against the same change.
//!
//! Before task 3, `content_pipeline` and `proposal_generator` returned
//! `None` from `profile_by_name("cheap-fast")` / `profile_by_name("thorough")`
//! — those two cells of the matrix are the ones this test exists to pin.
//!
//! The surface list below is explicit and closed. A tenth workflow surface
//! added later is NOT automatically covered — it must be added to
//! `surfaces()` below, or this test silently stops being exhaustive.
//!
//! `sdlc_flow` carries additional named profiles beyond the trio
//! (`pragmatist`, `batch_reviewer` — see `policy/profiles.rs`'s module
//! doc and `workflows::sdlc_flow::profiles`). Those are out of scope here:
//! this test asserts the trio resolves everywhere, not that the trio is
//! the complete profile set anywhere.
//!
//! # Shown capable of failing (carryover `gate-scope-must-be-shown-capable-of-failing`)
//!
//! This test's failure mode was demonstrated during EN.14.J task 4 by
//! temporarily removing one surface's `cheap-fast`/`thorough` arm from its
//! `profile_by_name` match (mirroring the pre-task-3 state) and confirming
//! this test goes red, naming the surface/profile pair, then restoring the
//! arm and re-confirming green. The observed before/after output is
//! recorded in the task 4 completion note — this is a demonstration
//! performed against the working tree, not a permanent second test, since
//! a test that undoes task 3's own fix to prove it matters would be
//! self-defeating to leave in the suite.

use engine_core::policy::resolve as resolve_policy;

const PROFILE_NAMES: [&str; 3] = ["baseline", "cheap-fast", "thorough"];

/// One cell-resolution attempt: look up `profile_name` on this surface via
/// its own `profile_by_name`, and if found, resolve it against that
/// surface's built-in default and confirm the resolved policy serializes.
/// Returns `true` only when both steps succeed — a missing profile name
/// (the pre-task-3 failure mode) and a resolve/serialize failure both
/// count as "resolution does not succeed".
type ResolveAttempt = fn(&str) -> bool;

fn surfaces() -> Vec<(&'static str, ResolveAttempt)> {
    use engine_core::workflows::{
        approve_and_run, content_pipeline, deliverable_render, diagnostic_intake, linkedin_post,
        proposal_generator, research_agent, sdlc_flow, sdlc_task,
    };

    vec![
        (
            "approve_and_run",
            (|name| {
                approve_and_run::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            approve_and_run::policy::ApproveAndRunPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "content_pipeline",
            (|name| {
                content_pipeline::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            content_pipeline::policy::ContentPipelinePolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "deliverable_render",
            (|name| {
                deliverable_render::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            deliverable_render::policy::DeliverableRenderPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "diagnostic_intake",
            (|name| {
                diagnostic_intake::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            diagnostic_intake::policy::DiagnosticIntakePolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "linkedin_post",
            (|name| {
                linkedin_post::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            linkedin_post::policy::LinkedInPostPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "proposal_generator",
            (|name| {
                proposal_generator::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            proposal_generator::policy::ProposalGeneratorPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "research_agent",
            (|name| {
                research_agent::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            research_agent::policy::ResearchAgentPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "sdlc_flow",
            (|name| {
                sdlc_flow::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            sdlc_flow::policy::SdlcPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
        (
            "sdlc_task",
            (|name| {
                sdlc_task::profiles::profile_by_name(name)
                    .map(|partial| {
                        serde_json::to_value(resolve_policy(
                            sdlc_task::policy::SdlcTaskPolicy::default(),
                            None,
                            Some(&partial),
                            None,
                        ))
                        .is_ok()
                    })
                    .unwrap_or(false)
            }),
        ),
    ]
}

/// The nine-surface x three-profile matrix: every cell must resolve
/// successfully. Failure output lists every failing (surface, profile)
/// pair rather than stopping at the first, so a run that breaks more than
/// one cell is diagnosed in one shot.
#[test]
fn every_surface_resolves_all_three_named_profiles() {
    let mut failures: Vec<String> = Vec::new();

    for (surface, attempt) in surfaces() {
        for profile in PROFILE_NAMES {
            if !attempt(profile) {
                failures.push(format!("{surface}/{profile}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "profile resolution failed for surface/profile pair(s): {}",
        failures.join(", ")
    );
}

/// Pins the matrix's own exhaustiveness: exactly nine surfaces are
/// covered. If this count changes, `surfaces()` above was edited without
/// updating this pin (or vice versa) — either way it needs a deliberate
/// look, not a silent pass.
#[test]
fn matrix_covers_exactly_the_nine_enumerated_surfaces() {
    assert_eq!(surfaces().len(), 9);
}
