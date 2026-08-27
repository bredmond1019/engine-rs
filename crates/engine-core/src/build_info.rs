//! Build/writer/host identity stamped into every artifact this engine writes.
//!
//! `GIT_SHA`/`BUILT_AT` come from `build.rs` via `env!` at compile time — see that file for the
//! git-unavailable / non-git-checkout fallback (the literal `"unknown"`). `WRITER` is the
//! discriminator that distinguishes this engine from base-template's JS `sdlc-flow.js`, which
//! shares `sdlc-flow-state.json` but never writes an equivalent key at all: a committed state
//! file with the key ABSENT is attributable to the JS engine, not to a third, unknown writer.

/// Full 40-character git SHA of the commit this binary was built from, or `"unknown"` when built
/// outside a git checkout (a vendored/CI tarball) or when `git` is unavailable on PATH.
pub const GIT_SHA: &str = env!("ENGINE_GIT_SHA");

/// RFC3339 UTC timestamp of when this binary was built, or `"unknown"` under the same fallback
/// conditions as [`GIT_SHA`].
pub const BUILT_AT: &str = env!("ENGINE_BUILT_AT");

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
}
