//! Pane bounding, redaction and hashing — pure functions (`EN.9.D` task 3).
//!
//! [`bound_pane_tail`] exists because `TaskContext` is serialized to jsonb
//! twice per node (once for the run-state snapshot, once for the
//! persistence write) with no bounding anywhere in the persistence path —
//! an unbounded captured pane (a runaway `tail -f`, a busy build log) would
//! otherwise be written to Postgres twice, unbounded, on every terminal
//! node. Caps default to 40 lines AND 8KB, whichever binds first
//! ([`DEFAULT_MAX_LINES`] / [`DEFAULT_MAX_BYTES`], bundled in
//! [`PaneLimits::default`]).
//!
//! Redaction runs BEFORE hashing (see [`bound_pane_tail`]'s body), so
//! `pane_sha256` is a hash of what is actually stored/observable, never of
//! pre-redaction text a caller with only the hash could not reconstruct
//! anyway — the ordering still matters because it is the difference between
//! "the hash proves what we redacted" and "the hash leaks what we redacted".
//!
//! [`PaneTailPolicy`] governs what [`bound_pane_tail`] actually stores.
//! [`default_pane_tail_policy`] resolves the CLAUDE.md standing-rule-6
//! built-in default: `HashOnly` when the owning session was adopted (an
//! adopted session may hold output this run never produced and has no
//! claim to persist as text), `Text` otherwise — behavior-stable because
//! every terminal node this block ships is new; there is no prior
//! `adopted: true` run whose output policy this default could silently
//! change out from under.

use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest as _, Sha256};

/// Default line cap: the tail is bounded to the LAST this-many lines.
pub const DEFAULT_MAX_LINES: usize = 40;

/// Default byte cap: the tail is bounded to the LAST this-many bytes,
/// applied after the line cap.
pub const DEFAULT_MAX_BYTES: usize = 8 * 1024;

/// The two caps [`bound_pane_tail`] applies — line count first, then byte
/// count on what the line cap left — either of which independently sets
/// `pane_truncated`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneLimits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for PaneLimits {
    fn default() -> Self {
        Self {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// What [`bound_pane_tail`] actually stores in the returned [`BoundedPane`].
/// `pane_truncated` and (when not [`PaneTailPolicy::None`]) `pane_sha256`
/// are computed regardless of policy — only which fields end up `Some`
/// differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneTailPolicy {
    /// Store neither the tail text nor its hash.
    None,
    /// Store the hash only — proves what was observed without persisting
    /// the text itself.
    HashOnly,
    /// Store both the tail text and its hash.
    Text,
}

impl PaneTailPolicy {
    /// The stable string form stamped into `ctx.nodes` (CLAUDE.md standing
    /// rule 6 — stamp the resolved policy value so `RunTelemetry` /
    /// `PolicyAggregate` can attribute observed behavior to the setting
    /// that caused it).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PaneTailPolicy::None => "none",
            PaneTailPolicy::HashOnly => "hash-only",
            PaneTailPolicy::Text => "text",
        }
    }
}

/// Resolve the built-in default [`PaneTailPolicy`] for a session: `HashOnly`
/// when the session was adopted (`adopted: true` — this run did not create
/// it and has no claim to persist output it did not produce), `Text`
/// otherwise.
#[must_use]
pub fn default_pane_tail_policy(adopted: bool) -> PaneTailPolicy {
    if adopted {
        PaneTailPolicy::HashOnly
    } else {
        PaneTailPolicy::Text
    }
}

/// The result of [`bound_pane_tail`]: what a terminal node stamps into
/// `ctx.nodes` for the pane it observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedPane {
    /// The bounded, redacted tail text — `Some` only under
    /// [`PaneTailPolicy::Text`].
    pub pane_tail: Option<String>,
    /// Hex SHA-256 of the bounded, redacted tail text — `Some` under
    /// [`PaneTailPolicy::HashOnly`] and [`PaneTailPolicy::Text`], `None`
    /// under [`PaneTailPolicy::None`].
    pub pane_sha256: Option<String>,
    /// `true` whenever either the line cap or the byte cap bound.
    pub pane_truncated: bool,
}

/// Secret-shaped `key: value` / `key=value` assignments — `api_key`,
/// `token`, `secret`, `password`/`passwd` (case-insensitive) — redacted
/// value-only so the key name (useful for triage) survives.
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(api[_-]?key|token|secret|passwd|password)(\s*[:=]\s*)(['"]?)([^\s'"]+)(['"]?)"#,
    )
    .expect("SECRET_ASSIGNMENT is a fixed, valid pattern")
});

/// `Authorization: Bearer <token>` / bare `Bearer <token>` — the token half
/// redacted, the scheme kept.
static BEARER_TOKEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9\-_.]+").expect("BEARER_TOKEN is a fixed, valid pattern")
});

/// Redact secret-shaped substrings out of `raw`. Deliberately pattern-based
/// rather than exhaustive — this is a best-effort pass over the two shapes
/// terminal output most commonly leaks (an assignment, an Authorization
/// header), not a general secret scanner.
fn redact(raw: &str) -> String {
    let redacted = SECRET_ASSIGNMENT.replace_all(raw, "$1$2$3[REDACTED]$5");
    let redacted = BEARER_TOKEN.replace_all(&redacted, "Bearer [REDACTED]");
    redacted.into_owned()
}

/// Hex-encoded SHA-256 of `s`.
fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Bound `raw` to `limits` (last N lines, then last N bytes of what
/// remains — whichever binds first sets `pane_truncated`), redact it, hash
/// the redacted result, then keep only what `policy` calls for.
#[must_use]
pub fn bound_pane_tail(raw: &str, policy: PaneTailPolicy, limits: PaneLimits) -> BoundedPane {
    let (line_bounded, line_truncated) = take_tail_lines(raw, limits.max_lines);
    let (byte_bounded, byte_truncated) = take_tail_bytes(&line_bounded, limits.max_bytes);
    let pane_truncated = line_truncated || byte_truncated;

    // Redaction BEFORE hashing (see module doc): `pane_sha256` must be the
    // hash of the post-redaction text, so it never proves a caller had
    // access to the pre-redaction secret.
    let redacted = redact(&byte_bounded);

    match policy {
        PaneTailPolicy::None => BoundedPane {
            pane_tail: None,
            pane_sha256: None,
            pane_truncated,
        },
        PaneTailPolicy::HashOnly => BoundedPane {
            pane_tail: None,
            pane_sha256: Some(sha256_hex(&redacted)),
            pane_truncated,
        },
        PaneTailPolicy::Text => BoundedPane {
            pane_sha256: Some(sha256_hex(&redacted)),
            pane_tail: Some(redacted),
            pane_truncated,
        },
    }
}

/// Keep only the last `max_lines` lines of `raw` (all of it if it already
/// has fewer). Returns the joined tail plus whether truncation occurred.
fn take_tail_lines(raw: &str, max_lines: usize) -> (String, bool) {
    let lines: Vec<&str> = raw.lines().collect();
    if lines.len() <= max_lines {
        (raw.to_string(), false)
    } else {
        let tail = &lines[lines.len() - max_lines..];
        (tail.join("\n"), true)
    }
}

/// Keep only the last `max_bytes` bytes of `s` (all of it if it already
/// fits), snapped forward to the nearest UTF-8 character boundary so the
/// cut never splits a multi-byte character. Returns the tail plus whether
/// truncation occurred.
fn take_tail_bytes(s: &str, max_bytes: usize) -> (String, bool) {
    if s.len() <= max_bytes {
        (s.to_string(), false)
    } else {
        let mut start = s.len() - max_bytes;
        while start < s.len() && !s.is_char_boundary(start) {
            start += 1;
        }
        (s[start..].to_string(), true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n_lines(n: usize) -> String {
        (0..n)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_2000_line_pane_is_bounded_and_marked_truncated() {
        let raw = n_lines(2000);
        let bounded = bound_pane_tail(&raw, PaneTailPolicy::Text, PaneLimits::default());
        assert!(bounded.pane_truncated);
        let tail = bounded.pane_tail.expect("Text policy stores tail");
        assert_eq!(tail.lines().count(), DEFAULT_MAX_LINES);
        // It is the LAST 40 lines, not the first 40.
        assert!(tail.starts_with("line 1960"));
        assert!(tail.ends_with("line 1999"));
    }

    #[test]
    fn byte_cap_binds_independently_of_line_cap_on_a_single_long_line() {
        // One line, well under the 40-line cap, but over the 8KB byte cap.
        let raw = "x".repeat(DEFAULT_MAX_BYTES * 2);
        let bounded = bound_pane_tail(&raw, PaneTailPolicy::Text, PaneLimits::default());
        assert!(bounded.pane_truncated);
        let tail = bounded.pane_tail.expect("Text policy stores tail");
        assert!(tail.len() <= DEFAULT_MAX_BYTES);
        assert_eq!(tail.lines().count(), 1);
    }

    #[test]
    fn untruncated_pane_is_not_marked_truncated() {
        let raw = "short pane\noutput";
        let bounded = bound_pane_tail(raw, PaneTailPolicy::Text, PaneLimits::default());
        assert!(!bounded.pane_truncated);
        assert_eq!(bounded.pane_tail.as_deref(), Some(raw));
    }

    #[test]
    fn none_policy_stores_neither_tail_nor_hash() {
        let bounded = bound_pane_tail("some output", PaneTailPolicy::None, PaneLimits::default());
        assert_eq!(bounded.pane_tail, None);
        assert_eq!(bounded.pane_sha256, None);
    }

    #[test]
    fn hash_only_policy_stores_hash_and_no_text() {
        let bounded = bound_pane_tail(
            "some output",
            PaneTailPolicy::HashOnly,
            PaneLimits::default(),
        );
        assert_eq!(bounded.pane_tail, None);
        assert!(bounded.pane_sha256.is_some());
    }

    #[test]
    fn text_policy_stores_both_tail_and_hash() {
        let bounded = bound_pane_tail("some output", PaneTailPolicy::Text, PaneLimits::default());
        assert!(bounded.pane_tail.is_some());
        assert!(bounded.pane_sha256.is_some());
    }

    #[test]
    fn pane_sha256_hashes_the_post_redaction_text_not_the_raw_text() {
        let raw = "api_key: super-secret-value-123\nother output line";
        let bounded = bound_pane_tail(raw, PaneTailPolicy::Text, PaneLimits::default());
        let tail = bounded.pane_tail.clone().expect("Text policy stores tail");

        // The stored tail itself must not contain the raw secret.
        assert!(!tail.contains("super-secret-value-123"));
        assert!(tail.contains("[REDACTED]"));

        // The hash must match a hash computed over the (same) redacted
        // text, not over `raw`.
        let expected = sha256_hex(&tail);
        assert_eq!(bounded.pane_sha256, Some(expected));

        // Pin that it is NOT simply the hash of the raw, pre-redaction text.
        let raw_hash = sha256_hex(raw);
        assert_ne!(bounded.pane_sha256, Some(raw_hash));
    }

    #[test]
    fn bearer_token_is_redacted() {
        let raw = "Authorization: Bearer abcDEF123.token-value_here";
        let redacted = redact(raw);
        assert!(!redacted.contains("abcDEF123.token-value_here"));
        assert!(redacted.contains("Bearer [REDACTED]"));
    }

    #[test]
    fn default_policy_is_hash_only_when_adopted_and_text_otherwise() {
        assert_eq!(default_pane_tail_policy(true), PaneTailPolicy::HashOnly);
        assert_eq!(default_pane_tail_policy(false), PaneTailPolicy::Text);
    }

    #[test]
    fn policy_as_str_is_stable() {
        assert_eq!(PaneTailPolicy::None.as_str(), "none");
        assert_eq!(PaneTailPolicy::HashOnly.as_str(), "hash-only");
        assert_eq!(PaneTailPolicy::Text.as_str(), "text");
    }
}
