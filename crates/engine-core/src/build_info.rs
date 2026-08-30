//! Build/writer/host identity stamped into every artifact this engine writes.
//!
//! `GIT_SHA`/`DIRTY`/`BUILT_AT` come from `build.rs` via `env!` at compile time — see that file
//! for the git-unavailable / non-git-checkout fallback (the literal `"unknown"`). `WRITER` is
//! the discriminator that distinguishes this engine from base-template's JS `sdlc-flow.js`,
//! which shares `sdlc-flow-state.json` but never writes an equivalent key at all: a committed
//! state file with the key ABSENT is attributable to the JS engine, not to a third, unknown
//! writer.
//!
//! `engine_build_sha()` is the single accessor `EN.ticket.stamp-engine-sha-on-every-run`
//! introduces: the run artifact (task 2) and `GET /health` (task 3) must both read the build
//! label through it rather than composing `GIT_SHA`/`DIRTY` themselves, so the two can never
//! drift apart. It reads no git subprocess at request time — every input is a `build.rs`
//! compile-time constant.

/// Full 40-character git SHA of the commit this binary was built from, or `"unknown"` when built
/// outside a git checkout (a vendored/CI tarball) or when `git` is unavailable on PATH.
pub const GIT_SHA: &str = env!("ENGINE_GIT_SHA");

/// `"1"` if the working tree had uncommitted changes at build time, `"0"` if it was clean,
/// `"unknown"` if the dirty check itself could not be run at build time — mirrors mev's
/// `MEV_BUILD_DIRTY` semantics (`mev conformance --check toolchain-freshness`) exactly, so a
/// binary's provenance reads the same way across both tools.
pub const DIRTY: &str = env!("ENGINE_GIT_DIRTY");

/// RFC3339 UTC timestamp of when this binary was built, or `"unknown"` under the same fallback
/// conditions as [`GIT_SHA`].
pub const BUILT_AT: &str = env!("ENGINE_BUILT_AT");

/// The single accessor for "which build produced this": the label the run artifact (task 2) and
/// `GET /health` (task 3) must both stamp, so the two sources cannot disagree.
///
/// - `GIT_SHA` unknown, or `DIRTY` unknown -> `"unknown"`. Provenance could not be established;
///   never guess.
/// - `DIRTY == "1"` -> `"<sha>-dirty"`. The SHA is known, but the tree that produced this binary
///   had uncommitted changes, so the SHA alone would misrepresent the build. A dirty build is
///   distinguishable from a clean one at this accessor, never silently reported as clean.
/// - Otherwise (`DIRTY == "0"`) -> `"<sha>"` verbatim, the ordinary clean-build case.
///
/// No git subprocess runs here or anywhere downstream of it — every input is a `build.rs`
/// compile-time constant baked in via `env!`.
pub fn engine_build_sha() -> String {
    format_build_sha(GIT_SHA, DIRTY)
}

/// Pure core of [`engine_build_sha`], taking the sha/dirty compile-time constants as plain
/// arguments so every branch (clean, dirty, unknown-sha, unknown-dirty) is directly unit
/// testable without depending on the actual build environment's git state.
fn format_build_sha(sha: &str, dirty: &str) -> String {
    if sha == "unknown" || dirty == "unknown" {
        "unknown".to_string()
    } else if dirty == "1" {
        format!("{sha}-dirty")
    } else {
        sha.to_string()
    }
}

/// Discriminator naming this engine as the writer of a committed artifact.
pub const WRITER: &str = "engine-rs";

/// This process's hostname and pid, carrying the single-host invariant into every artifact this
/// engine stamps. Falls back to the literal `"unknown"` hostname (never empty, never a lookup
/// failure surfaced to the caller) when neither `$HOSTNAME` nor the `hostname` binary is
/// available — no new workspace dependency was needed for this.
pub fn host_stamp() -> (String, u32) {
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());
    (hostname, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_sha_is_non_empty() {
        assert!(!GIT_SHA.is_empty());
    }

    #[test]
    fn built_at_parses_as_rfc3339_or_is_unknown() {
        if BUILT_AT != "unknown" {
            chrono::DateTime::parse_from_rfc3339(BUILT_AT)
                .unwrap_or_else(|e| panic!("BUILT_AT {:?} is not RFC3339: {e}", BUILT_AT));
        }
    }

    #[test]
    fn writer_is_engine_rs() {
        assert_eq!(WRITER, "engine-rs");
    }

    #[test]
    fn host_stamp_returns_current_pid_and_non_empty_hostname() {
        let (hostname, pid) = host_stamp();
        assert!(!hostname.is_empty());
        assert_eq!(pid, std::process::id());
    }

    #[test]
    fn dirty_is_a_recognized_value() {
        assert!(
            matches!(DIRTY, "0" | "1" | "unknown"),
            "DIRTY {:?} is neither \"0\", \"1\", nor \"unknown\" — build.rs's git_dirty() \
             contract is violated",
            DIRTY
        );
    }

    /// `engine_build_sha()` must be non-empty and, for the actual build that produced this test
    /// binary, must equal the same `GIT_SHA`/`DIRTY` constants `mev conformance --check
    /// toolchain-freshness` would compare against — no second, disagreeing notion of "which
    /// build am I" (EN.ticket.stamp-engine-sha-on-every-run task 1 acceptance criterion 1).
    #[test]
    fn engine_build_sha_is_non_empty_and_derived_from_the_same_constants() {
        let sha = engine_build_sha();
        assert!(!sha.is_empty());
        assert_eq!(sha, format_build_sha(GIT_SHA, DIRTY));
    }

    #[test]
    fn format_build_sha_clean_tree_is_the_bare_sha() {
        assert_eq!(format_build_sha("abc123", "0"), "abc123");
    }

    #[test]
    fn format_build_sha_dirty_tree_is_distinguishable_from_clean() {
        let clean = format_build_sha("abc123", "0");
        let dirty = format_build_sha("abc123", "1");
        assert_ne!(
            clean, dirty,
            "a dirty build must never format to the same label as a clean build with the same sha"
        );
        assert_eq!(dirty, "abc123-dirty");
        assert!(
            dirty.contains("abc123"),
            "the dirty label must still carry the real sha, not hide it"
        );
    }

    #[test]
    fn format_build_sha_unknown_sha_is_never_reported_as_clean_or_dirty() {
        assert_eq!(format_build_sha("unknown", "0"), "unknown");
        assert_eq!(format_build_sha("unknown", "1"), "unknown");
    }

    #[test]
    fn format_build_sha_unknown_dirty_state_is_unverifiable_not_clean() {
        // A known sha whose dirty-ness could not be determined must never be reported as if it
        // were verified clean — that would be a confident wrong label.
        assert_eq!(format_build_sha("abc123", "unknown"), "unknown");
    }
}
