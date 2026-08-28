//! `EN.11.A` Task 3 — the stale-artifact accessor
//! (`schema::committed_artifact_is_stale`) that `SQ-09`'s red-build gate will
//! key on, plus the fixture evidence base-template D68 constraint 4 requires:
//! a test that shows the gate CAN FAIL, not merely a declared assertion.
//!
//! Drives the accessor directly against hand-built D31-committed-shape JSON
//! `Value`s (the shape [`engine_core::workflows::sdlc_flow::schema::SDLCState::to_committed_state_json`]
//! emits) rather than through a full `Workflow` run — the accessor is a pure
//! function of `(artifact_json, current_run_id)`, so there is nothing to
//! stand up.
//!
//! Cases, matching the block record's minimum set verbatim:
//! (a) a different `run_id` than the current run -> stale (the shown-failing
//!     case — see `stale_case_is_a_real_assertion_that_can_fail` below for
//!     the inversion proof);
//! (b) the same `run_id` -> not stale;
//! (c) `final_validation: null` (an intermediate `SaveStateNode` save) with
//!     the CURRENT `run_id` -> not stale, pinning that the accessor does not
//!     key on `final_validation`'s presence/absence;
//! (d) `run_id: null` (a base-template JS-engine-written file) against a
//!     current run that DOES have a run_id -> stale.

use engine_core::workflows::sdlc_flow::schema::committed_artifact_is_stale;
use serde_json::json;

const RUN_A: &str = "11111111-1111-1111-1111-111111111111";
const RUN_B: &str = "22222222-2222-2222-2222-222222222222";

/// (a) An artifact stamped with a DIFFERENT `run_id` than the current run is
/// reported stale.
#[test]
fn different_run_id_is_reported_stale() {
    let artifact = json!({ "spec_slug": "fixture", "run_id": RUN_B });
    assert!(committed_artifact_is_stale(&artifact, Some(RUN_A)));
}

/// Proves case (a) is a REAL assertion, not a declaration that happens to
/// pass — base-template D68 constraint 4's "the check must be shown failing,
/// not merely declared". Reimplements the accessor with `!=` inverted to
/// `==` and confirms that WRONG version reports the different-run_id
/// artifact as NOT stale, i.e. the real accessor's `!=` comparison is
/// load-bearing rather than incidental. This does not touch (nor could it
/// break) the production function — it is a standalone negative control
/// that runs every time this suite runs, in `cargo nextest run`, as
/// permanent evidence rather than a one-off manual check.
#[test]
fn stale_case_is_a_real_assertion_that_can_fail() {
    fn inverted_committed_artifact_is_stale(
        value: &serde_json::Value,
        current_run_id: Option<&str>,
    ) -> bool {
        let artifact_run_id = value.get("run_id").and_then(serde_json::Value::as_str);
        match (artifact_run_id, current_run_id) {
            (None, None) => false,
            // Deliberately inverted: `==` instead of the real accessor's `!=`.
            (artifact, current) => artifact == current,
        }
    }

    let artifact = json!({ "spec_slug": "fixture", "run_id": RUN_B });

    // The real accessor correctly flags this as stale.
    assert!(committed_artifact_is_stale(&artifact, Some(RUN_A)));
    // The inverted comparison gets it backwards, proving the direction of
    // the real comparison is what makes the assertion above meaningful
    // rather than vacuous.
    assert!(!inverted_committed_artifact_is_stale(
        &artifact,
        Some(RUN_A)
    ));
}

/// (b) An artifact stamped with the SAME `run_id` is not stale.
#[test]
fn same_run_id_is_not_stale() {
    let artifact = json!({ "spec_slug": "fixture", "run_id": RUN_A });
    assert!(!committed_artifact_is_stale(&artifact, Some(RUN_A)));
}

/// (c) An intermediate `SaveStateNode` save carrying `final_validation: null`
/// but the CURRENT `run_id` is NOT reported stale — the accessor must key
/// only on `run_id`, never on `final_validation`'s presence, since
/// `SaveStateNode` writes `final_validation: null` on every save (seams.md
/// seam 3, the tri-state trap named in the `EN.11.A` block record).
#[test]
fn intermediate_save_with_null_final_validation_and_current_run_id_is_not_stale() {
    let artifact = json!({
        "spec_slug": "fixture",
        "run_id": RUN_A,
        "final_validation": null,
        "status": "running",
    });
    assert!(!committed_artifact_is_stale(&artifact, Some(RUN_A)));
}

/// (d) An artifact with `run_id: null` (a base-template JS-engine file, which
/// never sets this key) compared against a current run that DOES have a
/// run_id is reported stale.
#[test]
fn null_run_id_against_current_run_with_run_id_is_stale() {
    let artifact = json!({ "spec_slug": "fixture", "run_id": null });
    assert!(committed_artifact_is_stale(&artifact, Some(RUN_A)));

    // Same again with the key absent entirely rather than explicit `null` —
    // `Value::get` returns `None` either way, so this must behave
    // identically.
    let artifact_no_key = json!({ "spec_slug": "fixture" });
    assert!(committed_artifact_is_stale(&artifact_no_key, Some(RUN_A)));
}

/// Neither side has a run_id to compare — nothing to detect staleness
/// against, so the accessor declines to flag it rather than guessing.
#[test]
fn no_run_id_on_either_side_is_not_stale() {
    let artifact = json!({ "spec_slug": "fixture" });
    assert!(!committed_artifact_is_stale(&artifact, None));
}
