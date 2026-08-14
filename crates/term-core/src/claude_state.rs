// sessions/claude_state.rs — read-only observer for ~/.claude.json workspace trust.
//
// Decision D4: DB-free. No Config::load(), no Postgres pool.
// Decision D5: synchronous blocking — no tokio/async.
//
// This module is a pure read-only observer: it NEVER writes to ~/.claude.json
// and NEVER returns an error — Unknown is the safe fallback for any missing or
// malformed state.

use std::fmt;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Whether Claude Code has accepted the one-time workspace-trust prompt for a directory.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustStatus {
    /// `hasTrustDialogAccepted` is `true` for this directory.
    Trusted,
    /// `hasTrustDialogAccepted` is `false` for this directory.
    Untrusted,
    /// The file, project entry, or field is absent/unreadable/malformed.
    /// This is a normal, acceptable outcome — no error is surfaced.
    Unknown,
}

impl TrustStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustStatus::Trusted => "trusted",
            TrustStatus::Untrusted => "untrusted",
            TrustStatus::Unknown => "unknown",
        }
    }
}

impl fmt::Display for TrustStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── Pure parse function ───────────────────────────────────────────────────────

/// Parse the contents of `~/.claude.json` and return the trust status for `dir`.
///
/// Looks up `projects[dir].hasTrustDialogAccepted`:
/// - `true`  → `Trusted`
/// - `false` → `Untrusted`
/// - Missing project, missing field, non-bool, or unparseable JSON → `Unknown`
///
/// Never panics and never writes anything.
pub fn trust_for_dir(claude_json: &str, dir: &str) -> TrustStatus {
    // Fast path: empty input.
    if claude_json.trim().is_empty() {
        return TrustStatus::Unknown;
    }

    let v: serde_json::Value = match serde_json::from_str(claude_json) {
        Ok(v) => v,
        Err(_) => return TrustStatus::Unknown,
    };

    let accepted = v
        .get("projects")
        .and_then(|p| p.get(dir))
        .and_then(|e| e.get("hasTrustDialogAccepted"));

    match accepted {
        Some(serde_json::Value::Bool(true)) => TrustStatus::Trusted,
        Some(serde_json::Value::Bool(false)) => TrustStatus::Untrusted,
        _ => TrustStatus::Unknown,
    }
}

// ── Thin I/O shell (smoke-tested, not unit-tested) ────────────────────────────

/// Read `~/.claude.json` and return the trust status for `dir`.
///
/// If the file is absent, unreadable, or the HOME env var is missing,
/// returns `Unknown` (no error surfaced — this is a pre-flight advisory only).
pub fn trust_status(dir: &str) -> TrustStatus {
    let home = match std::env::var("HOME") {
        Ok(h) => h,
        Err(_) => return TrustStatus::Unknown,
    };

    let path = std::path::PathBuf::from(home).join(".claude.json");

    let contents = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return TrustStatus::Unknown,
    };

    trust_for_dir(&contents, dir)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const DIR: &str = "~/agentic-portfolio";

    fn json_with(accepted: bool) -> String {
        format!(
            r#"{{"projects": {{"{DIR}": {{"hasTrustDialogAccepted": {}}}}}}}"#,
            accepted
        )
    }

    // ── Happy paths ───────────────────────────────────────────────────────────

    #[test]
    fn trusted_dir_returns_trusted() {
        let json = json_with(true);
        assert_eq!(trust_for_dir(&json, DIR), TrustStatus::Trusted);
    }

    #[test]
    fn untrusted_dir_returns_untrusted() {
        let json = json_with(false);
        assert_eq!(trust_for_dir(&json, DIR), TrustStatus::Untrusted);
    }

    // ── Missing / absent ─────────────────────────────────────────────────────

    #[test]
    fn dir_absent_from_projects_returns_unknown() {
        let json = r#"{"projects": {"/other/dir": {"hasTrustDialogAccepted": true}}}"#;
        assert_eq!(trust_for_dir(json, DIR), TrustStatus::Unknown);
    }

    #[test]
    fn projects_key_absent_returns_unknown() {
        let json = r#"{"otherKey": {}}"#;
        assert_eq!(trust_for_dir(json, DIR), TrustStatus::Unknown);
    }

    #[test]
    fn has_trust_field_absent_returns_unknown() {
        let json = format!(r#"{{"projects": {{"{DIR}": {{"otherField": true}}}}}}"#);
        assert_eq!(trust_for_dir(&json, DIR), TrustStatus::Unknown);
    }

    // ── Type mismatches ───────────────────────────────────────────────────────

    #[test]
    fn non_bool_field_value_returns_unknown() {
        let json = format!(r#"{{"projects": {{"{DIR}": {{"hasTrustDialogAccepted": "yes"}}}}}}"#);
        assert_eq!(trust_for_dir(&json, DIR), TrustStatus::Unknown);
    }

    #[test]
    fn numeric_field_value_returns_unknown() {
        let json = format!(r#"{{"projects": {{"{DIR}": {{"hasTrustDialogAccepted": 1}}}}}}"#);
        assert_eq!(trust_for_dir(&json, DIR), TrustStatus::Unknown);
    }

    // ── Malformed / empty input ───────────────────────────────────────────────

    #[test]
    fn malformed_json_returns_unknown() {
        assert_eq!(trust_for_dir("not json {{{{", DIR), TrustStatus::Unknown);
    }

    #[test]
    fn empty_string_returns_unknown() {
        assert_eq!(trust_for_dir("", DIR), TrustStatus::Unknown);
    }

    #[test]
    fn whitespace_only_returns_unknown() {
        assert_eq!(trust_for_dir("   \n\t  ", DIR), TrustStatus::Unknown);
    }

    // ── Display / as_str ─────────────────────────────────────────────────────

    #[test]
    fn display_trusted() {
        assert_eq!(TrustStatus::Trusted.to_string(), "trusted");
    }

    #[test]
    fn display_untrusted() {
        assert_eq!(TrustStatus::Untrusted.to_string(), "untrusted");
    }

    #[test]
    fn display_unknown() {
        assert_eq!(TrustStatus::Unknown.to_string(), "unknown");
    }

    // ── No-write guarantee (structural) ──────────────────────────────────────
    //
    // trust_for_dir has no write path by construction: it takes &str and returns
    // TrustStatus without accepting any mutable state or file handle.  There is
    // nothing to test here beyond the fact that the function compiles and runs
    // without mutating input.
    #[test]
    fn trust_for_dir_does_not_mutate_input() {
        let json = json_with(true);
        let original = json.clone();
        let _ = trust_for_dir(&json, DIR);
        assert_eq!(json, original, "input was mutated");
    }
}
