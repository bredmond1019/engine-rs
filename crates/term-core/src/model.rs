// sessions/model.rs — Session / Pane types + pure parsing of tmux output.
//
// Parsing rule for running/idle indicator:
//   A session is considered "running" when its foreground pane process
//   (pane_current_command, field 5) is NOT an idle shell (zsh, bash, sh, fish).
//   Any other non-empty command → Running.  Empty or shell → Idle.
//   This is keyed on what is actually executing, not whether a client is attached,
//   so a detached-but-busy session (e.g. running `claude`) correctly reports Running.

use crate::tmux::{FIELD_SEP, LIST_SESSIONS_FORMAT};

/// Shell process names that indicate an idle (no foreground command) session.
/// Any `pane_current_command` in this set → `SessionState::Idle`.
/// Any other non-empty value → `SessionState::Running`.
const IDLE_SHELLS: &[&str] = &["zsh", "bash", "sh", "fish"];

/// Errors produced while parsing tmux `list-sessions` output.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("malformed list-sessions line (expected >=3 tab-separated fields): {0:?}")]
    TooFewFields(String),
    #[error("malformed list-sessions line: empty session name in {0:?}")]
    EmptyName(String),
}

/// Whether a session's foreground pane process is an active command or an idle shell.
#[derive(Debug, Clone, PartialEq)]
pub enum SessionState {
    Running,
    Idle,
}

/// Classify the foreground pane command as Running or Idle.
///
/// Rules:
/// - Command in `IDLE_SHELLS` (after trimming) → `Idle`
/// - Non-empty command not in IDLE_SHELLS → `Running`
/// - Empty / whitespace-only → `Idle` (conservative default)
pub fn classify_state(foreground_cmd: &str) -> SessionState {
    let cmd = foreground_cmd.trim();
    if cmd.is_empty() || IDLE_SHELLS.contains(&cmd) {
        SessionState::Idle
    } else {
        SessionState::Running
    }
}

impl SessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Running => "running",
            SessionState::Idle => "idle",
        }
    }
}

/// Represents a single tmux session as reported by `list-sessions`.
#[derive(Debug, Clone)]
pub struct Session {
    pub name: String,
    pub state: SessionState,
    pub window_count: u32,
    /// The foreground process name from `#{pane_current_command}`.
    /// Empty string when absent or unknown.
    pub foreground_cmd: String,
    /// Last non-blank line from `capture-pane -p`, empty string if none.
    pub last_line: String,
    /// Live agent state detected from pane content.
    pub agent_state: crate::detect::AgentState,
    /// Working directory of the session's first pane, from `#{pane_current_path}`.
    /// Empty string when absent (e.g. older/shorter list-sessions output).
    pub cwd: String,
    /// Sub-classifies `agent_state == Blocked` (e.g. a permission prompt vs. an
    /// `AskUserQuestion` prompt). `None` for every other `agent_state`, and also
    /// `None` when the session was parsed from `list-sessions` output alone with
    /// no pane capture to classify.
    pub blocked_reason: Option<crate::detect::BlockedReason>,
}

/// Lightweight pane capture: the raw text from `capture-pane -p -t <session>`.
/// The last non-blank line is extracted from this by `last_pane_line`.
#[derive(Debug, Clone)]
pub struct Pane {
    pub session_name: String,
    pub raw_output: String,
}

impl Pane {
    pub fn new(session_name: impl Into<String>, raw_output: impl Into<String>) -> Self {
        Self {
            session_name: session_name.into(),
            raw_output: raw_output.into(),
        }
    }

    /// Extract the last non-blank line from the captured pane output.
    pub fn last_line(&self) -> &str {
        self.raw_output
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
    }

    /// Return the trailing lines of pane output, after stripping trailing blank/whitespace-only
    /// lines that `tmux capture-pane -p` pads to fill the pane height.
    ///
    /// - `None`    → return all non-padding lines (oldest → newest).
    /// - `Some(n)` → return at most the last `n` non-padding lines.
    /// - `Some(0)` → empty Vec.
    pub fn last_lines(&self, n: Option<usize>) -> Vec<String> {
        if n == Some(0) {
            return Vec::new();
        }

        // Collect non-padding lines: strip trailing blank lines first.
        let lines: Vec<&str> = self.raw_output.lines().collect();
        let trimmed_end = lines
            .iter()
            .rposition(|l| !l.trim().is_empty())
            .map(|i| i + 1)
            .unwrap_or(0);
        let meaningful: &[&str] = &lines[..trimmed_end];

        match n {
            None => meaningful.iter().map(|l| l.to_string()).collect(),
            Some(count) => {
                let start = meaningful.len().saturating_sub(count);
                meaningful[start..].iter().map(|l| l.to_string()).collect()
            }
        }
    }
}

// ── Parsing ──────────────────────────────────────────────────────────────────

/// Parse a single `tmux list-sessions -F <LIST_SESSIONS_FORMAT>` output line
/// into a `Session` (without last-line, which requires a separate capture-pane call).
///
/// Expected field order (tab-separated, matches LIST_SESSIONS_FORMAT):
///   1. session_name
///   2. session_attached (1/0) — no longer the state source; kept for compatibility
///   3. session_windows
///   4. session_activity (epoch secs)
///   5. pane_current_command — the source of truth for Running vs Idle
///   6. pane_current_path — cwd of the first pane, used for session → space mapping
///
/// Returns `Err` for malformed lines so callers can choose to skip or propagate.
/// A line with fewer than 5 fields is accepted as long as ≥3 fields are present;
/// the missing 5th field defaults to empty (→ Idle), so older/shorter lines still parse.
/// The 6th field (cwd) defaults to empty when absent.
pub fn parse_session_line(line: &str) -> Result<Session, ModelError> {
    let _ = LIST_SESSIONS_FORMAT; // Suppress unused-import lint; used in doc / test.

    let parts: Vec<&str> = line.splitn(6, FIELD_SEP).collect();
    if parts.len() < 3 {
        return Err(ModelError::TooFewFields(line.to_string()));
    }

    let name = parts[0].to_string();
    if name.is_empty() {
        return Err(ModelError::EmptyName(line.to_string()));
    }

    let window_count: u32 = parts[2].trim().parse().unwrap_or(0);

    // Field 5 (index 4) is pane_current_command; absent → empty → Idle.
    let foreground_cmd = parts
        .get(4)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let state = classify_state(&foreground_cmd);

    // Field 6 (index 5) is pane_current_path (cwd); absent → empty.
    let cwd = parts
        .get(5)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Ok(Session {
        name,
        state,
        window_count,
        foreground_cmd,
        last_line: String::new(), // filled in by commands.rs after capture-pane
        agent_state: crate::detect::AgentState::Unknown,
        cwd,
        blocked_reason: None, // no pane capture here — this parser has no pane text to classify
    })
}

/// Parse the full multi-line output of `tmux list-sessions -F ...` into a
/// `Vec<Session>`. Malformed lines are skipped with a warning to stderr.
pub fn parse_sessions(output: &str) -> Vec<Session> {
    output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|line| match parse_session_line(line) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!("term-core: skipping malformed session line: {e}");
                None
            }
        })
        .collect()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Fixtures (5-field format: name, attached, windows, activity, pane_cmd) ─

    /// Fixture: two sessions — one running a command (detached), one idle shell.
    const FIXTURE_TWO_SESSIONS: &str = "\
main\t0\t3\t1718000000\tclaude\n\
background\t0\t1\t1718000100\tzsh\n";

    /// Fixture: single session running a foreground command.
    const FIXTURE_RUNNING: &str = "work\t0\t2\t1718000200\tcargo\n";

    /// Fixture: single session at an idle shell.
    const FIXTURE_IDLE: &str = "scratch\t0\t1\t1718000300\tzsh\n";

    /// Fixture: malformed line (only one field).
    const FIXTURE_MALFORMED: &str = "bad-line-no-tabs\n";

    // ── classify_state ────────────────────────────────────────────────────────

    #[test]
    fn classify_zsh_is_idle() {
        assert_eq!(classify_state("zsh"), SessionState::Idle);
    }

    #[test]
    fn classify_bash_is_idle() {
        assert_eq!(classify_state("bash"), SessionState::Idle);
    }

    #[test]
    fn classify_sh_is_idle() {
        assert_eq!(classify_state("sh"), SessionState::Idle);
    }

    #[test]
    fn classify_fish_is_idle() {
        assert_eq!(classify_state("fish"), SessionState::Idle);
    }

    #[test]
    fn classify_claude_is_running() {
        assert_eq!(classify_state("claude"), SessionState::Running);
    }

    #[test]
    fn classify_node_is_running() {
        assert_eq!(classify_state("node"), SessionState::Running);
    }

    #[test]
    fn classify_cargo_is_running() {
        assert_eq!(classify_state("cargo"), SessionState::Running);
    }

    #[test]
    fn classify_vim_is_running() {
        assert_eq!(classify_state("vim"), SessionState::Running);
    }

    #[test]
    fn classify_empty_is_idle() {
        assert_eq!(classify_state(""), SessionState::Idle);
    }

    #[test]
    fn classify_whitespace_only_is_idle() {
        assert_eq!(classify_state("   "), SessionState::Idle);
    }

    #[test]
    fn classify_trims_whitespace_before_comparing() {
        // Leading/trailing whitespace must not prevent shell detection.
        assert_eq!(classify_state("  zsh  "), SessionState::Idle);
    }

    // ── Session line parsing ──────────────────────────────────────────────────

    #[test]
    fn parses_5_field_running_command() {
        let s = parse_session_line("work\t0\t2\t1718000200\tcargo").unwrap();
        assert_eq!(s.name, "work");
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.window_count, 2);
        assert_eq!(s.foreground_cmd, "cargo");
    }

    #[test]
    fn parses_5_field_idle_shell() {
        let s = parse_session_line("scratch\t0\t1\t1718000300\tzsh").unwrap();
        assert_eq!(s.name, "scratch");
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.foreground_cmd, "zsh");
    }

    #[test]
    fn detached_running_command_classifies_as_running() {
        // Core bug fix: a detached session (attached==0) running `claude` must report Running.
        let s = parse_session_line("ai\t0\t1\t1718000000\tclaude").unwrap();
        assert_eq!(s.state, SessionState::Running);
        assert_eq!(s.foreground_cmd, "claude");
    }

    #[test]
    fn detached_idle_shell_classifies_as_idle() {
        let s = parse_session_line("shell\t0\t1\t1718000000\tzsh").unwrap();
        assert_eq!(s.state, SessionState::Idle);
    }

    #[test]
    fn missing_5th_field_defaults_to_idle() {
        // A 4-field line (old format) should parse as Idle, not error.
        let s = parse_session_line("old\t0\t1\t1718000000").unwrap();
        assert_eq!(s.state, SessionState::Idle);
        assert_eq!(s.foreground_cmd, "");
    }

    #[test]
    fn parses_6_field_line_with_cwd() {
        let s = parse_session_line("work\t0\t2\t1718000200\tcargo\t/Users/dev/repo").unwrap();
        assert_eq!(s.name, "work");
        assert_eq!(s.foreground_cmd, "cargo");
        assert_eq!(s.cwd, "/Users/dev/repo");
    }

    #[test]
    fn missing_6th_field_defaults_cwd_to_empty() {
        // A 5-field line (no pane_current_path) should parse without error, cwd empty.
        let s = parse_session_line("work\t0\t2\t1718000200\tcargo").unwrap();
        assert_eq!(s.cwd, "");
    }

    // ── blocked_reason ────────────────────────────────────────────────────────

    #[test]
    fn parse_session_line_has_no_blocked_reason_without_pane_capture() {
        // `list-sessions` output alone carries no pane text, so there is nothing
        // to classify — blocked_reason must be None regardless of foreground_cmd.
        let s = parse_session_line("work\t0\t2\t1718000200\tclaude").unwrap();
        assert_eq!(s.blocked_reason, None);
    }

    #[test]
    fn captured_pane_holding_ask_user_question_threads_awaiting_question_reason() {
        // Mirrors what the tmux polling path does after a real `capture-pane`:
        // run detection on the raw pane text and copy both `state` and
        // `blocked_reason` onto the Session, without re-deriving the reason
        // from pane text a second time.
        const CLAUDE_MANIFEST: &str = include_str!("detect/manifests/claude.toml");
        let manifest = crate::detect::manifest::parse_manifest(CLAUDE_MANIFEST)
            .expect("claude.toml parse failed")
            .compile()
            .expect("claude.toml compile failed");

        let pane = include_str!("detect/fixtures/claude_awaiting_question.txt");
        let detection = crate::detect::detect(pane, &manifest);

        let mut s = parse_session_line("ai\t0\t1\t1718000000\tclaude").unwrap();
        assert_eq!(s.blocked_reason, None); // pre-capture: no pane text yet
        s.agent_state = detection.state;
        s.blocked_reason = detection.blocked_reason;

        assert_eq!(s.agent_state, crate::detect::AgentState::Blocked);
        assert_eq!(
            s.blocked_reason,
            Some(crate::detect::BlockedReason::AwaitingQuestion)
        );
    }

    #[test]
    fn parses_multiple_sessions() {
        let sessions = parse_sessions(FIXTURE_TWO_SESSIONS);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].name, "main");
        assert_eq!(sessions[0].state, SessionState::Running); // claude → running
        assert_eq!(sessions[1].name, "background");
        assert_eq!(sessions[1].state, SessionState::Idle); // zsh → idle
    }

    #[test]
    fn malformed_line_is_skipped_not_panicked() {
        // The malformed line should be silently skipped; valid lines still parsed.
        let input = format!(
            "{}\n{}",
            FIXTURE_MALFORMED.trim(),
            "good\t0\t1\t1718000000\tzsh"
        );
        let sessions = parse_sessions(&input);
        // Only the good line should be returned.
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].name, "good");
    }

    #[test]
    fn empty_output_yields_empty_vec() {
        let sessions = parse_sessions("");
        assert!(sessions.is_empty());
    }

    // ── Running / idle fixture helpers ────────────────────────────────────────

    #[test]
    fn running_fixture_is_running() {
        let sessions = parse_sessions(FIXTURE_RUNNING);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Running);
    }

    #[test]
    fn idle_fixture_is_idle() {
        let sessions = parse_sessions(FIXTURE_IDLE);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].state, SessionState::Idle);
    }

    // ── Pane / last-line ─────────────────────────────────────────────────────

    #[test]
    fn pane_last_line_returns_last_nonblank() {
        let pane = Pane::new("work", "first line\nsecond line\n\n");
        assert_eq!(pane.last_line(), "second line");
    }

    #[test]
    fn pane_last_line_empty_when_all_blank() {
        let pane = Pane::new("empty-session", "\n   \n\n");
        assert_eq!(pane.last_line(), "");
    }

    #[test]
    fn pane_last_line_single_line() {
        let pane = Pane::new("single", "only line");
        assert_eq!(pane.last_line(), "only line");
    }

    #[test]
    fn state_as_str() {
        assert_eq!(SessionState::Running.as_str(), "running");
        assert_eq!(SessionState::Idle.as_str(), "idle");
    }

    // ── Pane::last_lines ─────────────────────────────────────────────────────

    #[test]
    fn last_lines_none_returns_all_nonblank_trailing_stripped() {
        let pane = Pane::new("s", "line1\nline2\nline3\n\n   \n");
        let lines = pane.last_lines(None);
        assert_eq!(lines, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn last_lines_some_n_more_lines_than_n() {
        let pane = Pane::new("s", "a\nb\nc\nd\ne\n\n");
        let lines = pane.last_lines(Some(3));
        assert_eq!(lines, vec!["c", "d", "e"]);
    }

    #[test]
    fn last_lines_some_n_fewer_lines_than_n() {
        let pane = Pane::new("s", "x\ny\n\n\n");
        let lines = pane.last_lines(Some(10));
        assert_eq!(lines, vec!["x", "y"]);
    }

    #[test]
    fn last_lines_some_n_exactly_n_lines() {
        let pane = Pane::new("s", "p\nq\nr\n\n");
        let lines = pane.last_lines(Some(3));
        assert_eq!(lines, vec!["p", "q", "r"]);
    }

    #[test]
    fn last_lines_some_zero_returns_empty() {
        let pane = Pane::new("s", "a\nb\nc\n");
        let lines = pane.last_lines(Some(0));
        assert!(lines.is_empty());
    }

    #[test]
    fn last_lines_empty_input_returns_empty() {
        let pane = Pane::new("s", "");
        assert!(pane.last_lines(None).is_empty());
        assert!(pane.last_lines(Some(5)).is_empty());
    }

    #[test]
    fn last_lines_all_blank_returns_empty() {
        let pane = Pane::new("s", "\n   \n\t\n");
        assert!(pane.last_lines(None).is_empty());
        assert!(pane.last_lines(Some(3)).is_empty());
    }

    #[test]
    fn last_lines_order_is_preserved_oldest_newest() {
        let pane = Pane::new("s", "first\nsecond\nthird\n\n");
        let lines = pane.last_lines(Some(2));
        assert_eq!(lines, vec!["second", "third"]);
    }

    #[test]
    fn last_lines_trailing_blank_padding_stripped() {
        // Simulate tmux padding: content lines followed by many blank lines.
        let raw = "output1\noutput2\n\n\n\n\n\n\n\n";
        let pane = Pane::new("s", raw);
        let lines = pane.last_lines(None);
        assert_eq!(lines, vec!["output1", "output2"]);
    }
}
