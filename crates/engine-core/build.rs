//! Stamps the build that produces this binary: the git sha it was built from and an RFC3339
//! UTC build timestamp, both exposed to `engine_core::build_info` via `env!`.
//!
//! Falls back to the literal `"unknown"` when `git` is unavailable on PATH or the build runs
//! outside a git checkout (a vendored/CI tarball) — a build that cannot be stamped must still
//! build, per EN.11.A task 1.

use std::process::Command;

fn git_sha() -> String {
    Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn build_timestamp() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn main() {
    println!("cargo:rustc-env=ENGINE_GIT_SHA={}", git_sha());
    println!("cargo:rustc-env=ENGINE_BUILT_AT={}", build_timestamp());

    // Re-stamp whenever HEAD moves (new commit, checkout, merge) or a ref updates (branch
    // fast-forward/reset). Both paths are relative to this crate's manifest dir
    // (`crates/engine-core`), two levels below the workspace/repo root.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
}
