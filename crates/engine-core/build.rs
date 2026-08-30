//! Stamps the build that produces this binary: the git sha it was built from, whether the
//! working tree was dirty at build time, and an RFC3339 UTC build timestamp — all exposed to
//! `engine_core::build_info` via `env!`.
//!
//! Falls back to the literal `"unknown"` when `git` is unavailable on PATH or the build runs
//! outside a git checkout (a vendored/CI tarball) — a build that cannot be stamped must still
//! build, per EN.11.A task 1.
//!
//! `ENGINE_GIT_DIRTY` mirrors mev's `MEV_BUILD_DIRTY` semantics exactly (`MV.ticket
//! .toolchain-freshness-covers-the-writer`'s stamp, read by `mev conformance --check
//! toolchain-freshness`): `"1"` if `git status --porcelain` was non-empty at build time, `"0"`
//! if it was clean, `"unknown"` if the dirty check itself could not be run. A binary built from
//! a dirty tree must never carry a SHA that reads as clean — that is a confident wrong label,
//! worse than an absent one (`EN.ticket.stamp-engine-sha-on-every-run` task 1).

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

/// `"1"` if the working tree had uncommitted changes at build time, `"0"` if clean, `"unknown"`
/// if `git status --porcelain` could not be run (no `git` on PATH, or not a checkout).
fn git_dirty() -> String {
    Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|s| if s.trim().is_empty() { "0" } else { "1" }.to_string())
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
    println!("cargo:rustc-env=ENGINE_GIT_DIRTY={}", git_dirty());
    println!("cargo:rustc-env=ENGINE_BUILT_AT={}", build_timestamp());

    // Re-stamp whenever HEAD moves (new commit, checkout, merge), a ref updates (branch
    // fast-forward/reset), or the index changes (the dirty flag depends on working-tree state,
    // not just HEAD). All paths are relative to this crate's manifest dir
    // (`crates/engine-core`), two levels below the workspace/repo root.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-changed=../../.git/index");
}
