// detect/golden_tests.rs — seed manifest + fixture golden tests.
//
// Owned by spec Task 2. Task 1 pre-declared this module slot; Task 2 fills it.
//
// All manifests and fixtures are loaded via `include_str!` — zero filesystem
// I/O at test time, so these tests run anywhere cargo runs.
//
// Extensibility assertion (spec AC): Adding the Pi manifest required ONLY
// a new TOML file + fixture + this test file — no changes to the detection
// engine (`mod.rs` / `manifest.rs`). That property is demonstrated below:
// the Pi tests are indistinguishable from the Claude tests in their use of
// the engine API; the engine has no knowledge of either agent.

use crate::detect::manifest::parse_manifest;
use crate::detect::{detect, AgentState};

// ── Load manifests and fixtures at compile time ───────────────────────────────

const CLAUDE_MANIFEST: &str = include_str!("manifests/claude.toml");
const PI_MANIFEST: &str = include_str!("manifests/pi.toml");

const CLAUDE_BLOCKED_FIXTURE: &str = include_str!("fixtures/claude_blocked.txt");
const CLAUDE_WORKING_FIXTURE: &str = include_str!("fixtures/claude_working.txt");
const CLAUDE_IDLE_FIXTURE: &str = include_str!("fixtures/claude_idle.txt");
const PI_WORKING_FIXTURE: &str = include_str!("fixtures/pi_working.txt");
const PI_IDLE_FIXTURE: &str = include_str!("fixtures/pi_idle.txt");

// ── Claude golden tests ───────────────────────────────────────────────────────

/// Claude approval dialog → Blocked + visible_blocker.
#[test]
fn claude_blocked_fixture_yields_blocked_with_visible_blocker() {
    let manifest = parse_manifest(CLAUDE_MANIFEST)
        .expect("claude.toml parse failed")
        .compile()
        .expect("claude.toml compile failed");

    let detection = detect(CLAUDE_BLOCKED_FIXTURE, &manifest);

    assert_eq!(
        detection.state,
        AgentState::Blocked,
        "expected Blocked, got {:?}",
        detection.state
    );
    assert!(
        detection.visible_blocker,
        "expected visible_blocker == true"
    );
    assert!(
        !detection.visible_working,
        "visible_working should be false"
    );
    assert!(!detection.visible_idle, "visible_idle should be false");
}

/// Claude working screen → Working + visible_working.
#[test]
fn claude_working_fixture_yields_working() {
    let manifest = parse_manifest(CLAUDE_MANIFEST)
        .expect("claude.toml parse failed")
        .compile()
        .expect("claude.toml compile failed");

    let detection = detect(CLAUDE_WORKING_FIXTURE, &manifest);

    assert_eq!(detection.state, AgentState::Working);
    assert!(detection.visible_working);
    assert!(!detection.visible_blocker);
}

/// Claude idle prompt → Idle + visible_idle.
#[test]
fn claude_idle_fixture_yields_idle() {
    let manifest = parse_manifest(CLAUDE_MANIFEST)
        .expect("claude.toml parse failed")
        .compile()
        .expect("claude.toml compile failed");

    let detection = detect(CLAUDE_IDLE_FIXTURE, &manifest);

    assert_eq!(detection.state, AgentState::Idle);
    assert!(detection.visible_idle);
    assert!(!detection.visible_blocker);
    assert!(!detection.visible_working);
}

// ── Pi golden tests ───────────────────────────────────────────────────────────
//
// EXTENSIBILITY NOTE: The Pi manifest was added by creating `pi.toml` +
// `fixtures/pi_*.txt` + these test cases only. Not a single line of
// `mod.rs` or `manifest.rs` was changed. This is the agent-agnostic seam
// the block is designed to establish (spec AC, Task 2 "zero engine-code
// change" assertion).

/// Pi working screen → Working + visible_working.
#[test]
fn pi_working_fixture_yields_working() {
    let manifest = parse_manifest(PI_MANIFEST)
        .expect("pi.toml parse failed")
        .compile()
        .expect("pi.toml compile failed");

    let detection = detect(PI_WORKING_FIXTURE, &manifest);

    assert_eq!(
        detection.state,
        AgentState::Working,
        "expected Working, got {:?}",
        detection.state
    );
    assert!(detection.visible_working);
}

/// Pi idle screen → Idle + visible_idle.
#[test]
fn pi_idle_fixture_yields_idle() {
    let manifest = parse_manifest(PI_MANIFEST)
        .expect("pi.toml parse failed")
        .compile()
        .expect("pi.toml compile failed");

    let detection = detect(PI_IDLE_FIXTURE, &manifest);

    assert_eq!(detection.state, AgentState::Idle);
    assert!(detection.visible_idle);
}

// ── Cross-agent isolation ─────────────────────────────────────────────────────

/// A Claude screen run through the Pi manifest should return Unknown — the
/// manifests are agent-scoped and do not bleed across agents.
#[test]
fn claude_blocked_screen_through_pi_manifest_is_unknown() {
    let manifest = parse_manifest(PI_MANIFEST)
        .expect("pi.toml parse failed")
        .compile()
        .expect("pi.toml compile failed");

    let detection = detect(CLAUDE_BLOCKED_FIXTURE, &manifest);

    // The Claude approval box contains "Do you want to proceed?" which the Pi
    // manifest does not recognize — expect Unknown with all flags false.
    assert_eq!(detection.state, AgentState::Unknown);
    assert!(!detection.visible_blocker);
    assert!(!detection.visible_working);
    assert!(!detection.visible_idle);
}
