//! `AwaitPredicate` and its pure evaluation (`EN.9.E` task 1).
//!
//! Every variant here is decided by a PURE function over an already-taken
//! [`Observation`] — no file IO, no clock reads, no driver calls. The
//! caller (`TerminalAwaitNode`, task 3) is responsible for taking the
//! observation (reading the marker file, capturing the pane, measuring
//! silence, checking the process exit code) once per poll tick and handing
//! it to [`evaluate`]. This split is what makes the marker semantics and
//! the four detect/regex/silence/exit-code rules unit-testable without a
//! real tmux session, a real clock, or a real filesystem race.
//!
//! # Marker semantics — all four parts load-bearing
//!
//! [`AwaitPredicate::Marker`] is satisfied only when ALL of the following
//! hold, checked against the observed [`MarkerObservation`]:
//!
//! 1. **Path.** The marker lives at [`marker_path`]`(out, nonce)` —
//!    `{out}.{nonce}.done`. The caller is responsible for reading exactly
//!    that path; this module only computes it.
//! 2. **Content equals the nonce.** A path that exists but holds a
//!    DIFFERENT nonce is another run's marker (a prior send using the same
//!    `out` file, or a colliding pane) and must not satisfy this await.
//! 3. **Never `remove_file`.** This module performs no file IO at all, so
//!    it cannot delete anything — but the design constraint is recorded
//!    here because it drives point 4: deletion would race a concurrent
//!    reader and make a stale marker indistinguishable from an absent one,
//!    so mtime is the only safe staleness signal and it must survive.
//! 4. **`out`'s mtime postdates the send.** A marker file surviving from a
//!    previous run has an OLDER mtime than this run's send and must not
//!    satisfy a fresh await — this is what makes a stale marker rejectable
//!    even when its content happens to collide (e.g. a reused nonce in a
//!    test fixture). The comparison is against [`Observation::sent_at`],
//!    strictly-greater-than: a marker written in the exact same instant as
//!    the send is treated as stale (conservative — a real send always
//!    takes nonzero time before the marker can be written in response).
//!
//! # Why an enum, not "just use Detect"
//!
//! Only 3 of `term_core`'s detect rules are live and all 3 are literal
//! `contains` matches (`claude.toml`'s two `blocked` rules and its one
//! `working` rule) — the fourth, `idle`, is the only rule that uses a
//! regex. A pane running something Claude Code's manifest never describes
//! (a bare `cargo build` pane, a shell script) matches none of them and
//! classifies [`term_core::detect::AgentState::Unknown`] FOREVER — there is
//! no timeout or fallback that promotes `Unknown` to any other state
//! (`term_core::detect` module doc). An await that can only wait on
//! `Detect` could therefore never terminate on a build pane; `Marker`,
//! `Regex`, `Silence`, and `ExitCode` exist because `Detect` alone is not
//! sufficient for the panes this engine actually drives.

use std::time::{Duration, SystemTime};

use regex::Regex;
use term_core::detect::manifest::{parse_manifest, CompiledManifest};
use term_core::detect::{detect, AgentState, CLAUDE_MANIFEST_TOML};

/// What a `TerminalAwaitNode` poll tick is waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AwaitPredicate {
    /// Wait for the nonce'd marker file at `{out}.{nonce}.done` — see the
    /// module doc's "Marker semantics" section for all four load-bearing
    /// rules.
    Marker { out: String, nonce: String },
    /// Wait for `term_core::detect` to classify the captured screen as
    /// `target`. Subject to the `Unknown`-forever gap documented in the
    /// module doc — callers driving a plain shell command should prefer
    /// `Marker`.
    Detect { target: AgentState },
    /// Wait for `pattern` to match anywhere in the captured screen.
    Regex { pattern: String },
    /// Wait until the pane has produced no new output for at least
    /// `min_duration`.
    Silence { min_duration: Duration },
    /// Wait for the driven process to have exited. `expected: Some(code)`
    /// requires that exact exit code; `None` is satisfied by any exit.
    ExitCode { expected: Option<i32> },
}

/// The marker file's observed state, as read by the caller immediately
/// before calling [`evaluate`]. `content`/`mtime` are `None` when the
/// marker path does not exist (or could not be read) — a read error is
/// treated identically to "does not exist yet", never as satisfying.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MarkerObservation {
    pub exists: bool,
    pub content: Option<String>,
    pub mtime: Option<SystemTime>,
}

/// One poll tick's worth of observed state, handed to [`evaluate`]. The
/// caller takes this fresh each tick; nothing here is mutated or reused
/// across ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Observation {
    /// The captured pane screen this tick.
    pub screen: String,
    /// The marker file's state, for [`AwaitPredicate::Marker`].
    pub marker: MarkerObservation,
    /// How long the pane has shown no new output, for
    /// [`AwaitPredicate::Silence`].
    pub silence_duration: Duration,
    /// The driven process's exit code, if it has exited, for
    /// [`AwaitPredicate::ExitCode`].
    pub exit_code: Option<i32>,
    /// When the triggering `TerminalSendNode` sent its command — the
    /// baseline [`AwaitPredicate::Marker`]'s mtime check compares against.
    pub sent_at: SystemTime,
}

/// Whether a poll tick's [`Observation`] satisfies an [`AwaitPredicate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PredicateOutcome {
    /// The predicate is satisfied — the await node should stop polling and
    /// return.
    Satisfied,
    /// Not yet satisfied — the await node should poll again (subject to
    /// its own timeout and cancellation, task 3).
    Pending,
}

impl PredicateOutcome {
    #[must_use]
    pub fn is_satisfied(self) -> bool {
        matches!(self, PredicateOutcome::Satisfied)
    }
}

/// Compute the marker path for a send targeting `out` with the given
/// `nonce`: `{out}.{nonce}.done`. The sole place this format string is
/// written — both the sender (task 2, which must write here) and the
/// evaluator (below, which must read here) go through this function so the
/// two can never drift apart.
#[must_use]
pub fn marker_path(out: &str, nonce: &str) -> String {
    format!("{out}.{nonce}.done")
}

/// The production Claude detect manifest, compiled once and reused across
/// every [`evaluate`] call for [`AwaitPredicate::Detect`] — mirrors
/// `observe.rs`'s `CLAUDE_MANIFEST` static (`EN.9.D` task 4), kept as an
/// independent copy here so this module stays a self-contained pure-logic
/// unit rather than reaching into a sibling node module for it.
static CLAUDE_MANIFEST: std::sync::LazyLock<CompiledManifest> = std::sync::LazyLock::new(|| {
    parse_manifest(CLAUDE_MANIFEST_TOML)
        .expect("CLAUDE_MANIFEST_TOML is a fixed, valid manifest")
        .compile()
        .expect("CLAUDE_MANIFEST_TOML's rules are fixed, valid gates")
});

/// Pure evaluation: does `observation` satisfy `predicate`? No IO, no
/// clock reads — everything needed is already inside `observation`.
#[must_use]
pub fn evaluate(predicate: &AwaitPredicate, observation: &Observation) -> PredicateOutcome {
    let satisfied = match predicate {
        AwaitPredicate::Marker { out, nonce } => marker_satisfied(out, nonce, observation),
        AwaitPredicate::Detect { target } => {
            detect(&observation.screen, &CLAUDE_MANIFEST).state == *target
        }
        AwaitPredicate::Regex { pattern } => Regex::new(pattern)
            .map(|re| re.is_match(&observation.screen))
            .unwrap_or(false),
        AwaitPredicate::Silence { min_duration } => observation.silence_duration >= *min_duration,
        AwaitPredicate::ExitCode { expected } => match (expected, observation.exit_code) {
            (Some(want), Some(got)) => want == &got,
            (None, Some(_)) => true,
            (_, None) => false,
        },
    };
    if satisfied {
        PredicateOutcome::Satisfied
    } else {
        PredicateOutcome::Pending
    }
}

/// [`AwaitPredicate::Marker`]'s four-part check — see the module doc's
/// "Marker semantics" section. `out`/`nonce` are only used to state intent
/// at the call site (the actual path match already happened when the
/// caller decided which file to read into `observation.marker`); the
/// content-equals-nonce and mtime-postdates-send checks are what this
/// function actually enforces.
fn marker_satisfied(_out: &str, nonce: &str, observation: &Observation) -> bool {
    if !observation.marker.exists {
        return false;
    }
    let content_matches = observation
        .marker
        .content
        .as_deref()
        .is_some_and(|content| content == nonce);
    if !content_matches {
        return false;
    }
    observation
        .marker
        .mtime
        .is_some_and(|mtime| mtime > observation.sent_at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_observation(sent_at: SystemTime) -> Observation {
        Observation {
            screen: String::new(),
            marker: MarkerObservation::default(),
            silence_duration: Duration::ZERO,
            exit_code: None,
            sent_at,
        }
    }

    // ── Marker: the four load-bearing rules ─────────────────────────────

    #[test]
    fn marker_stale_right_path_older_mtime_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.marker = MarkerObservation {
            exists: true,
            content: Some("nonce-1".to_string()),
            // Written before the send — a marker surviving from a
            // previous run.
            mtime: Some(sent_at - Duration::from_secs(60)),
        };
        let predicate = AwaitPredicate::Marker {
            out: "/tmp/out.log".to_string(),
            nonce: "nonce-1".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    #[test]
    fn marker_foreign_content_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.marker = MarkerObservation {
            exists: true,
            content: Some("some-other-runs-nonce".to_string()),
            mtime: Some(sent_at + Duration::from_secs(1)),
        };
        let predicate = AwaitPredicate::Marker {
            out: "/tmp/out.log".to_string(),
            nonce: "nonce-1".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    #[test]
    fn marker_fresh_matching_satisfies() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.marker = MarkerObservation {
            exists: true,
            content: Some("nonce-1".to_string()),
            mtime: Some(sent_at + Duration::from_secs(1)),
        };
        let predicate = AwaitPredicate::Marker {
            out: "/tmp/out.log".to_string(),
            nonce: "nonce-1".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn marker_absent_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let obs = base_observation(sent_at);
        let predicate = AwaitPredicate::Marker {
            out: "/tmp/out.log".to_string(),
            nonce: "nonce-1".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    #[test]
    fn marker_path_format() {
        assert_eq!(
            marker_path("/tmp/out.log", "abc123"),
            "/tmp/out.log.abc123.done"
        );
    }

    // ── Detect ───────────────────────────────────────────────────────────

    #[test]
    fn detect_matching_state_satisfies() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.screen = "some text\n> ".to_string();
        let predicate = AwaitPredicate::Detect {
            target: AgentState::Idle,
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn detect_unknown_screen_never_satisfies_a_non_unknown_target() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.screen = "$ cargo build\n   Compiling foo v0.1.0\n".to_string();
        let predicate = AwaitPredicate::Detect {
            target: AgentState::Idle,
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    // ── Regex ────────────────────────────────────────────────────────────

    #[test]
    fn regex_matching_satisfies() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.screen = "Build succeeded in 1.2s".to_string();
        let predicate = AwaitPredicate::Regex {
            pattern: r"Build succeeded".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn regex_non_matching_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.screen = "still working...".to_string();
        let predicate = AwaitPredicate::Regex {
            pattern: r"Build succeeded".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    #[test]
    fn regex_invalid_pattern_never_satisfies() {
        let sent_at = SystemTime::now();
        let obs = base_observation(sent_at);
        let predicate = AwaitPredicate::Regex {
            pattern: r"(unclosed".to_string(),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    // ── Silence ──────────────────────────────────────────────────────────

    #[test]
    fn silence_at_or_past_threshold_satisfies() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.silence_duration = Duration::from_secs(5);
        let predicate = AwaitPredicate::Silence {
            min_duration: Duration::from_secs(5),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn silence_below_threshold_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.silence_duration = Duration::from_millis(500);
        let predicate = AwaitPredicate::Silence {
            min_duration: Duration::from_secs(5),
        };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    // ── ExitCode ─────────────────────────────────────────────────────────

    #[test]
    fn exit_code_matching_expected_satisfies() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.exit_code = Some(0);
        let predicate = AwaitPredicate::ExitCode { expected: Some(0) };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn exit_code_mismatched_expected_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.exit_code = Some(1);
        let predicate = AwaitPredicate::ExitCode { expected: Some(0) };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }

    #[test]
    fn exit_code_any_satisfies_once_exited() {
        let sent_at = SystemTime::now();
        let mut obs = base_observation(sent_at);
        obs.exit_code = Some(137);
        let predicate = AwaitPredicate::ExitCode { expected: None };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Satisfied);
    }

    #[test]
    fn exit_code_not_yet_exited_does_not_satisfy() {
        let sent_at = SystemTime::now();
        let obs = base_observation(sent_at);
        let predicate = AwaitPredicate::ExitCode { expected: None };
        assert_eq!(evaluate(&predicate, &obs), PredicateOutcome::Pending);
    }
}
