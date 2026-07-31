//! The SDLC Flow (`SDLC_FLOW`) workflow — a Rust port of the Python
//! `orchestrator/app/workflows/sdlc_flow_workflow.py` pipeline's top half:
//! setup → generate/load tasks → the implement/test/triage/review task loop
//! with its runtime retry back-edges.
//!
//! Module layout (each leaf file owned by exactly one task in
//! `planning/EN.3.A-sdlc-flow-setup-task-loop/tasks.json` or
//! `planning/EN.3.B-sdlc-flow-docs-wrapup-pr/tasks.json`):
//! - `schema` — the ported `SDLCState`/`SDLCTask`/`SDLCFlowEventSchema` types.
//! - `setup` — `SetupWorktreeNode` / `SpecExistsRouterNode` /
//!   `GenerateTasksNode` / `LoadTaskStateNode`.
//! - `task_loop` — the implement→test→triage→review→update/save loop nodes
//!   and routers.
//! - `docs` — `PatchDocsNode` (bottom-half, EN.3.B).
//! - `wrap_up` — `WrapUpNode` (bottom-half, EN.3.B).
//! - `pr` — `PullRequestNode` (bottom-half, EN.3.B).
//! - `emit_state` — `EmitStateNode` (bottom-half, EN.3.B).
//! - `final_validation` — `FinalValidationNode`: the unconditional run-level
//!   full-suite gate on the task-loop drain branch (`EN.3.E`).
//! - `graph` — assembles the declared `WorkflowSchema` + `NodeRegistry` for
//!   the whole workflow.
//! - `aggregate` — the cross-run `(policy -> cost, time, quality)`
//!   aggregator (EN.3.C task 7): reads a set of `sdlc-flow-state.json`
//!   snapshots and tabulates one row per distinct resolved policy.
//!
//! The node-plumbing seams shared by every submodule — `CommandOutput` /
//! `CommandRunner` / `default_command_runner` — are owned here so every leaf
//! module imports a single definition via `super::...` (hoisted in EN.3.B
//! task 1 out of `setup.rs`/`task_loop.rs`, which had byte-identical private
//! copies). `ModelTransport`, the `put_result`/`get_result` context helpers,
//! `strip_json_fence`, and `parse_structured_or_fenced` were hoisted one
//! level further, up to `workflows::mod` (EN.4.0 task 4), since they are not
//! SDLC-specific; this module re-exports them so every existing
//! `super::`/`sdlc_flow::` import site keeps resolving unchanged.

use std::path::Path;
use std::sync::Arc;

// `strip_json_fence` has no direct callers left in this module now that
// `parse_structured_or_fenced` (also re-exported here) is the sole caller,
// but it stays re-exported for back-compat — any `super::strip_json_fence`
// import site elsewhere in the crate must keep resolving unchanged.
pub use super::ModelTransport;
#[allow(unused_imports)]
pub(crate) use super::{get_result, parse_structured_or_fenced, put_result, strip_json_fence};

pub mod aggregate;
pub mod docs;
pub mod emit_state;
pub mod final_validation;
pub mod graph;
pub mod policy;
pub mod pr;
pub mod profiles;
pub mod schema;
pub mod setup;
pub mod task_loop;
pub mod wrap_up;

/// Result of running a single shell command via the injectable
/// [`CommandRunner`] seam.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    /// Process exit status (`-1` when the platform reports no code).
    pub status: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The injectable command-runner signature nodes use to invoke subprocesses
/// (`git`, `gh`, `mev`, ...). Defaults to the real subprocess via
/// [`default_command_runner`]; tests substitute a stub so the gated
/// `cargo test` suite never shells out — mirrors
/// `ClaudeCodeStep::with_transport` (EN.2.A).
pub type CommandRunner =
    Arc<dyn Fn(&str, &[&str], &Path) -> std::io::Result<CommandOutput> + Send + Sync>;

/// The default [`CommandRunner`]: shells out to the real subprocess via
/// `std::process::Command`.
#[must_use]
pub fn default_command_runner() -> CommandRunner {
    Arc::new(|program, args, cwd| {
        let output = std::process::Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()?;
        Ok(CommandOutput {
            status: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    })
}

/// Git-add + git-commit `state_path` inside `worktree` via `runner`, routing
/// a non-zero commit outcome through [`log_noop_commit`] rather than
/// treating it as a node failure (e.g. "nothing to commit" on a re-save of
/// an unchanged file). Extracted (EN.3.G task 3) out of
/// `task_loop::SaveStateNode::process`'s formerly-inline tail so
/// `wrap_up::WrapUpNode` can commit its own terminal state write through the
/// exact same seam instead of duplicating it — `SaveStateNode`'s observable
/// behavior (same two subprocess invocations, same argv, same cwd, same
/// non-fatal treatment of a non-zero commit) is unchanged by the move.
pub(crate) fn commit_state_file(runner: &CommandRunner, worktree: &Path, state_path: &Path) {
    let state_path_str = state_path.to_string_lossy().to_string();
    let _ = runner("git", &["add", &state_path_str], worktree);
    let commit = runner(
        "git",
        &["commit", "-m", "chore: flow state update"],
        worktree,
    );
    if let Ok(output) = &commit {
        if output.status != 0 {
            // "nothing to commit" or an equivalent no-op — logged, not
            // an error, mirroring `save_state_node.py`.
            log_noop_commit(&output.stderr);
        }
    }
}

/// Best-effort no-op logging hook for a non-fatal `git commit` outcome
/// (e.g. "nothing to commit, working tree clean"). Kept as a tiny named
/// function rather than an inline `eprintln!` so its intent — "logged, not
/// an error" — reads at the call site. Currently a stub (filled in by
/// EN.3.G task 4 to distinguish a genuine no-op from a real commit
/// failure).
fn log_noop_commit(_stderr: &str) {}
