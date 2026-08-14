// detect/mod.rs — pure, config-driven agent-state detection.
//
// Per-agent TOML manifests (see manifest.rs) compile into priority-ordered rules;
// `detect()` resolves each rule's screen region and evaluates its gate, returning
// the first matching rule's outcome. Clean-room reimplementation of the Herdr
// detect *pattern* (Herdr is AGPL-3.0 — reference only, no copied source).
//
// **Only two detection rules are live today:** `manifests/claude.toml` is the
// production manifest; `manifests/pi.toml` exists purely as a second fixture to
// prove the engine is agent-agnostic (see `golden_tests.rs`) and is not wired to
// any real agent. A pane whose captured screen matches no rule in the active
// manifest classifies `Unknown` **forever** — there is no timeout or fallback
// that promotes it to any other state. `EN.9.E` depends on knowing this: it is
// the single most likely way to build something that looks finished (compiles,
// passes tests, wired end to end) and then silently never fires because the
// manifest it is driven by has no rule that matches the screens it actually sees.

pub mod manifest;

#[cfg(test)]
mod golden_tests; // slot owned by spec Task 2 (manifests + fixtures + golden tests)

use manifest::{resolve_region, CompiledManifest};
use serde::{Deserialize, Serialize};

// ── Core types ────────────────────────────────────────────────────────────────

/// Classified state of an agent session from its captured pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl AgentState {
    /// Human-readable lowercase name for this state.
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentState::Idle => "idle",
            AgentState::Working => "working",
            AgentState::Blocked => "blocked",
            AgentState::Unknown => "unknown",
        }
    }
}

/// Full detection outcome: the classified state plus the visibility and control
/// flags carried by the matching rule. On no match: `Unknown` with all flags `false`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentDetection {
    pub state: AgentState,
    /// Show the "idle" UI indicator.
    pub visible_idle: bool,
    /// Show the "blocker / needs input" UI indicator.
    pub visible_blocker: bool,
    /// Show the "working" UI indicator.
    pub visible_working: bool,
    /// When `true`, the caller should not write a new state record.
    pub skip_state_update: bool,
}

impl AgentDetection {
    /// The sentinel value returned when no rule in the manifest matches.
    pub fn unknown() -> Self {
        Self {
            state: AgentState::Unknown,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_state_update: false,
        }
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Evaluate `manifest`'s compiled rules (sorted descending by priority) against
/// `screen`. Returns the first matching rule's `AgentDetection`, or
/// `AgentDetection::unknown()` when no rule matches.
pub fn detect(screen: &str, manifest: &CompiledManifest) -> AgentDetection {
    for rule in &manifest.rules {
        let region = resolve_region(screen, &rule.region);
        if rule.gate.eval(&region) {
            return AgentDetection {
                state: rule.state,
                visible_idle: rule.visible_idle,
                visible_blocker: rule.visible_blocker,
                visible_working: rule.visible_working,
                skip_state_update: rule.skip_state_update,
            };
        }
    }
    AgentDetection::unknown()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use manifest::parse_manifest;

    // ── AgentState::as_str round-trip ─────────────────────────────────────────

    #[test]
    fn as_str_idle() {
        assert_eq!(AgentState::Idle.as_str(), "idle");
    }

    #[test]
    fn as_str_working() {
        assert_eq!(AgentState::Working.as_str(), "working");
    }

    #[test]
    fn as_str_blocked() {
        assert_eq!(AgentState::Blocked.as_str(), "blocked");
    }

    #[test]
    fn as_str_unknown() {
        assert_eq!(AgentState::Unknown.as_str(), "unknown");
    }

    // ── detect() — priority ordering ──────────────────────────────────────────

    /// A higher-priority rule must win even when a lower-priority rule also matches.
    #[test]
    fn detect_returns_first_matching_rule_by_priority() {
        // Both rules match "working idle"; the blocked rule (priority 100) should win.
        let src = r#"
name = "test"

[[rules]]
state = "idle"
priority = 1
visible_idle = true
gate = { contains = "idle" }

[[rules]]
state = "blocked"
priority = 100
visible_blocker = true
gate = { contains = "working" }
"#;
        let manifest = parse_manifest(src)
            .expect("parse failed")
            .compile()
            .expect("compile failed");

        let detection = detect("working idle session", &manifest);
        assert_eq!(detection.state, AgentState::Blocked);
        assert!(detection.visible_blocker);
        assert!(!detection.visible_idle);
    }

    // ── detect() — no-match → Unknown ────────────────────────────────────────

    #[test]
    fn detect_no_match_returns_unknown() {
        let src = r#"
name = "test"

[[rules]]
state = "idle"
gate = { contains = "NEVER_PRESENT_IN_SCREEN" }
"#;
        let manifest = parse_manifest(src)
            .expect("parse failed")
            .compile()
            .expect("compile failed");

        let detection = detect("some unrelated screen content", &manifest);
        assert_eq!(detection, AgentDetection::unknown());
    }

    // ── detect() — skip_state_update flag carry-through ──────────────────────

    #[test]
    fn detect_carries_skip_state_update_flag() {
        let src = r#"
name = "test"

[[rules]]
state = "working"
skip_state_update = true
gate = { contains = "spinner" }
"#;
        let manifest = parse_manifest(src)
            .expect("parse failed")
            .compile()
            .expect("compile failed");

        let detection = detect("spinner animation active", &manifest);
        assert_eq!(detection.state, AgentState::Working);
        assert!(detection.skip_state_update);
    }

    // ── detect() — empty manifest → Unknown ──────────────────────────────────

    #[test]
    fn detect_empty_manifest_returns_unknown() {
        let src = r#"name = "test""#;
        let manifest = parse_manifest(src)
            .expect("parse failed")
            .compile()
            .expect("compile failed");

        let detection = detect("any screen content here", &manifest);
        assert_eq!(detection, AgentDetection::unknown());
    }
}
