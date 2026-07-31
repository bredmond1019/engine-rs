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
            log_noop_commit(state_path, output);
        }
    }
}

/// Pure classifier for a non-zero `git commit` exit: `true` when the
/// stdout/stderr text describes the ordinary "nothing to commit" outcome
/// (re-saving an unchanged file), `false` for anything else (a genuine
/// failure). Split out as a small pure function — rather than folded into
/// [`log_noop_commit`] — so tests can assert on the classification directly
/// instead of capturing `eprintln!` output.
pub(crate) fn is_noop_commit(stderr: &str, stdout: &str) -> bool {
    let haystack = format!("{stdout}\n{stderr}").to_lowercase();
    haystack.contains("nothing to commit")
        || haystack.contains("working tree clean")
        || haystack.contains("no changes added to commit")
}

/// Logging hook for a non-zero `git commit` outcome from
/// [`commit_state_file`], distinguishing the ordinary no-op ("nothing to
/// commit, working tree clean") from a genuine failure — that distinction is
/// the entire point of this function; do not collapse it back to a single
/// branch.
///
/// **Why the quiet path matters in THIS repo specifically:** `planning/` is
/// a gitignored symlink (`.gitignore` line 7 `/planning`, `planning ->
/// ../_planning/engine-rs`), so every single state commit in this tree is a
/// no-op. A blanket warn on every non-zero exit would therefore fire on
/// every task of every run and train the reader to ignore it — hence the
/// no-op branch is silent by default and only prints when `ENGINE_DEBUG` is
/// set, while a genuine failure always prints (with the stderr text and the
/// state path) regardless.
///
/// Uses `eprintln!`, not a logging facade: the workspace carries no
/// `tracing`/`log` dependency in any `crates/*/Cargo.toml` nor the workspace
/// root (verified during EN.3.G authoring), and adding one for a single call
/// site is out of scope here.
fn log_noop_commit(state_path: &Path, output: &CommandOutput) {
    if is_noop_commit(&output.stderr, &output.stdout) {
        if std::env::var("ENGINE_DEBUG").is_ok() {
            eprintln!(
                "sdlc_flow: state commit no-op ({}): {}",
                state_path.display(),
                output.stderr.trim()
            );
        }
    } else {
        eprintln!(
            "sdlc_flow: WARNING state commit failed ({}): {}",
            state_path.display(),
            output.stderr.trim()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::is_noop_commit;

    #[test]
    fn is_noop_commit_classifies_nothing_to_commit_as_a_noop() {
        assert!(is_noop_commit("nothing to commit, working tree clean", ""));
    }

    #[test]
    fn is_noop_commit_classifies_no_changes_added_as_a_noop() {
        assert!(is_noop_commit(
            "",
            "no changes added to commit (use \"git add\" and/or \"git commit -a\")"
        ));
    }

    #[test]
    fn is_noop_commit_classifies_working_tree_clean_as_a_noop_case_insensitively() {
        assert!(is_noop_commit("Working Tree Clean", ""));
    }

    #[test]
    fn is_noop_commit_classifies_a_genuine_failure_as_not_a_noop() {
        assert!(!is_noop_commit("fatal: unable to write new index file", ""));
    }

    #[test]
    fn is_noop_commit_classifies_unrelated_stderr_as_not_a_noop() {
        assert!(!is_noop_commit(
            "error: pathspec did not match any files",
            ""
        ));
    }
}
