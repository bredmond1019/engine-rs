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
//! - `close_block` — `CloseBlockNode`: closes this run's block in
//!   `planning/state.json` through mev, under mev's advisory lock and D71
//!   operator gate (`EN.ticket.wrap-up-closes-the-block`).
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
#[allow(unused_imports)]
pub(crate) use super::{get_result, parse_structured_or_fenced, put_result, strip_json_fence};
pub use super::{ModelTransport, TransportSlot};
#[allow(unused_imports)]
pub use crate::policy::command_floor::{self, evaluate_command, CommandDecision};

pub mod aggregate;
pub mod close_block;
pub mod docs;
pub mod emit_state;
pub mod end_review;
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
/// `std::process::Command` — gated by the non-overridable
/// [`command_floor::evaluate_command`] org-floor denylist. Every consumer of
/// this seam (git/gh argv from `setup.rs`/`pr.rs`, `sh -c` harness-check
/// strings from `task_loop.rs`) funnels through here, so gating it here
/// covers all of them with no per-call-site changes (see
/// `planning/ticket-sdlc-command-policy-floor/tasks.md`). A denied command
/// never reaches `std::process::Command`.
#[must_use]
pub fn default_command_runner() -> CommandRunner {
    Arc::new(|program, args, cwd| {
        let joined = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        if let CommandDecision::Deny { reason, matched } = command_floor::evaluate_command(&joined)
        {
            return Ok(CommandOutput {
                status: 126,
                stdout: String::new(),
                stderr: format!("command-policy: blocked ({reason}): {matched}"),
            });
        }
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

/// Stage **everything** in `worktree` (`git add -A`) and commit it with
/// `message`, routing a non-zero commit outcome through [`log_noop_commit`]
/// rather than treating it as a node failure (e.g. "nothing to commit" when
/// the tree is already clean).
///
/// Supersedes the former `commit_state_file`, which staged only the state
/// file's own path. That narrowness was the root cause behind four
/// independent SDLC_FLOW defects: nothing in the run ever committed the
/// implementer's CODE, so the consolidated review saw an empty diff, the
/// trivial-skip classifier always counted zero changed lines, every
/// auto-PR pushed a branch of state-file-only commits, and `PatchDocsNode`'s
/// doc edits (`docs.rs` makes no git calls at all) never reached the branch.
///
/// **The commit topology this establishes** — `HEAD` carries every completed
/// task's code; the working tree delta vs `HEAD` is exactly the current
/// task's in-progress work. `SaveStateNode` runs only on the pass path, so
/// one passed task is one commit; the retry path never reaches it, so
/// retries accumulate uncommitted and each attempt's review sees the
/// cumulative attempt via `git diff HEAD`.
///
/// Whether the state file rides along in that commit is repo-dependent: in
/// **this** repo `planning/` is a gitignored symlink into a brain vault
/// (`.gitignore` `/planning`), so `add -A` cannot stage
/// `planning/<slug>/sdlc/sdlc-flow-state.json` and every commit here carries
/// code only. In a repo that tracks `planning/`, the state file is included.
/// Do not read the doc comments elsewhere in this module as promising the
/// state file is committed in engine-rs — it is not, and was not before this
/// helper widened either.
///
/// **Why `add -A` and not an explicit file list:** `.gitignore` guards build
/// artifacts, and `TestTaskNode::changed_files` already treats untracked
/// paths as expected implementer output — an explicit list would silently
/// drop any file the agent created but did not "claim".
///
/// # Blast radius — this is tree-wide, and `use_worktree` defaults to FALSE
///
/// `SDLCFlowEventSchema::use_worktree` is `#[serde(default)]` **false**, and
/// on that path `SetupWorktreeNode` checks the run's branch out **in a live
/// checkout** (`worktree_path == "."`, or the registry-resolved repo root)
/// rather than under `trees/<branch>`. `add -A` there stages *every* dirty
/// path in that checkout, including edits the operator made and never
/// intended to hand to the run.
///
/// **Guarded since `ticket-setup-rs-closeout`:** on that path
/// `SetupWorktreeNode` now runs `git status --porcelain` first and aborts the
/// run — naming the dirty paths — when the tree is not clean, mirroring
/// `.claude/workflows/sdlc-flow.js`'s branch-mode guard. So `add -A` here can
/// only ever sweep a tree that was clean when the run started, plus whatever
/// the run itself produced. A `use_worktree: true` run is isolated and
/// unguarded by design.
///
/// Prefer `use_worktree: true` anyway for any run you do not want touching
/// the ambient tree at all.
///
/// Returns `true` when the commit actually landed. A `false` means `HEAD` did
/// **not** advance, which silently breaks the topology invariant for the next
/// task (its `git diff HEAD` would then include this task's work too), so
/// callers stamp the outcome where telemetry can see it rather than
/// discarding it.
pub(crate) fn commit_all(runner: &CommandRunner, worktree: &Path, message: &str) -> bool {
    let _ = runner("git", &["add", "-A"], worktree);
    let commit = runner("git", &["commit", "-m", message], worktree);
    match &commit {
        Ok(output) if output.status == 0 => true,
        Ok(output) => {
            // "nothing to commit" or an equivalent no-op — logged, not
            // an error, mirroring `save_state_node.py`.
            log_noop_commit(message, output);
            false
        }
        Err(_) => false,
    }
}

/// Pure classifier for a non-zero `git commit` exit: `true` when the
/// stdout/stderr text describes the ordinary "nothing to commit" outcome
/// (re-saving an unchanged file), `false` for anything else (a genuine
/// failure). Split out as a small pure function — rather than folded into
/// [`log_noop_commit`] — so tests can assert on the classification directly
/// instead of capturing `tracing` output.
pub(crate) fn is_noop_commit(stderr: &str, stdout: &str) -> bool {
    let haystack = format!("{stdout}\n{stderr}").to_lowercase();
    haystack.contains("nothing to commit")
        || haystack.contains("working tree clean")
        || haystack.contains("no changes added to commit")
}

/// Logging hook for a non-zero `git commit` outcome from
/// [`commit_all`], distinguishing the ordinary no-op ("nothing to
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
/// Uses `tracing`'s `debug!`/`warn!` (EN.11.I migrated this off `eprintln!`;
/// the workspace now carries `tracing` as a workspace dependency).
/// `label` is a human-readable identifier for the commit that no-opped — the
/// commit message since the widening to [`commit_all`], the state file's path
/// before it. It exists only for this diagnostic; nothing parses it.
fn log_noop_commit(label: &str, output: &CommandOutput) {
    if is_noop_commit(&output.stderr, &output.stdout) {
        if std::env::var("ENGINE_DEBUG").is_ok() {
            tracing::debug!(
                label = %label,
                stderr = %output.stderr.trim(),
                "sdlc_flow: state commit no-op"
            );
        }
    } else {
        tracing::warn!(
            label = %label,
            stderr = %output.stderr.trim(),
            "sdlc_flow: state commit failed"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{default_command_runner, is_noop_commit};

    #[test]
    fn default_command_runner_blocks_a_denied_command_without_spawning() {
        let runner = default_command_runner();
        // A nonexistent cwd proves no real subprocess ran: if the deny path
        // fell through to `std::process::Command`, `current_dir` would fail
        // and this call would return an `Err`, not an `Ok` with status 126.
        let bogus_cwd = std::path::Path::new("/no/such/directory/for/this/test");
        let output = runner("git", &["push", "--force"], bogus_cwd)
            .expect("denied command must short-circuit before spawning, not error");
        assert_eq!(output.status, 126);
        assert!(
            output.stderr.contains("force push"),
            "stderr should name the deny reason: {}",
            output.stderr
        );
        assert!(
            output.stderr.contains("git push --force"),
            "stderr should include the matched text: {}",
            output.stderr
        );
        assert!(output.stdout.is_empty());
    }

    #[test]
    fn default_command_runner_allows_an_ordinary_command_unaffected() {
        let runner = default_command_runner();
        let tmp = std::env::temp_dir();
        let output = runner("echo", &["hi"], &tmp).expect("echo should run normally");
        assert_eq!(output.status, 0);
        assert!(output.stdout.contains("hi"));
        assert!(output.stderr.is_empty());
    }

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
