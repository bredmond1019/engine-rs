//! Concrete workflows built on the `engine-core` `Node`/`Router`/`Workflow`
//! primitives. Each submodule owns one ported workflow graph.
//!
//! The model-node seams shared across every workflow — `ModelTransport`, the
//! `ctx.nodes` helpers `put_result`/`get_result`, `strip_json_fence`, and the
//! `parse_structured_or_fenced` structured-output-preferred-over-fence parse
//! pattern (repeated across `ImplementTaskNode`/`TriageTaskNode`/
//! `ConsolidatedReviewNode`/`GenerateTasksNode`/`PatchDocsNode` — see
//! `EN.1-plan.A`) — live here so any future workflow can reuse them without
//! depending on `sdlc_flow`. `CommandOutput`/`CommandRunner`/
//! `default_command_runner`/`commit_all`/`is_noop_commit` also live here
//! (hoisted out of `sdlc_flow` in `EN.11.M` task 2, so a second engine can
//! shell out through the same injectable, org-floor-gated seam without
//! depending on `sdlc_flow`); `sdlc_flow` re-exports the hoisted seams so
//! existing `super::`/`sdlc_flow::` import sites resolve unchanged (EN.4.0
//! task 4, EN.11.M task 2).

use std::path::Path;
use std::sync::Arc;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use futures::future::BoxFuture;

use crate::policy::command_floor::{self, CommandDecision};

pub mod approve_and_run;
pub mod content_pipeline;
pub mod deliverable_render;
pub mod diagnostic_intake;
pub mod harvest_approve;
pub mod lead_ingest;
pub mod linkedin_post;
pub mod opportunity_edit;
pub mod orchestration;
pub mod proposal_generator;
pub mod research_agent;
pub mod sdlc_flow;
pub mod sdlc_task;
pub mod terminal_probe;
pub mod transport_slot;

pub use transport_slot::TransportSlot;

/// The injectable transport signature for model-calling nodes' composed
/// `ClaudeCodeStep`s — identical shape to `ClaudeCodeStep`'s own (private)
/// transport type. Defaults to the real `claude_code_rs::execute`; tests
/// substitute a stub via each node's `with_transport`.
pub type ModelTransport = Arc<
    dyn Fn(Config, String) -> BoxFuture<'static, claude_code_rs::Result<Outcome>> + Send + Sync,
>;

/// Stamp a node's output onto `ctx.nodes` under its own identity.
pub(crate) fn put_result(ctx: &mut TaskContext, identity: &str, value: serde_json::Value) {
    ctx.nodes.insert(identity.to_string(), value);
}

/// Look up a prior node's output from `ctx.nodes` by identity.
pub(crate) fn get_result<'a>(
    ctx: &'a TaskContext,
    identity: &str,
) -> Option<&'a serde_json::Value> {
    ctx.nodes.get(identity)
}

/// Strip a Markdown code fence (` ```json ... ``` ` or plain ` ``` ... ``` `)
/// wrapping a model's reply, if present, so a strict `serde_json::from_str`
/// parse still succeeds. Every model node here prompts for "strict JSON",
/// but a real `claude` response commonly wraps it in a fence anyway
/// (observed live, `EN.3.C`+ manual verification) — this is the one
/// normalization applied before every model-output JSON parse in this
/// module. Returns the input unchanged (just trimmed) when no fence is
/// present, so a genuinely bare JSON reply round-trips exactly as before.
pub(crate) fn strip_json_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let Some(after_open) = trimmed.strip_prefix("```") else {
        return trimmed;
    };
    // Drop an optional language tag (e.g. `json`) up to the first newline.
    let after_lang = match after_open.find('\n') {
        Some(idx) => &after_open[idx + 1..],
        None => after_open,
    };
    match after_lang.rfind("```") {
        Some(idx) => after_lang[..idx].trim(),
        None => trimmed,
    }
}

/// Prefer the pre-parsed `structured` value written by a `ClaudeCodeStep`
/// (stamped onto `ctx.nodes[node_name]["structured"]`) when present and
/// non-null; otherwise fall back to [`strip_json_fence`] +
/// `serde_json::from_str` on the raw text `content`. Factored out of the
/// byte-identical copies `ImplementTaskNode`/`TriageTaskNode`/
/// `ConsolidatedReviewNode` (`task_loop.rs`), `GenerateTasksNode`
/// (`setup.rs`), and `PatchDocsNode` (`docs.rs`) each carried privately
/// (EN.4.0 task 4).
pub(crate) fn parse_structured_or_fenced<T: serde::de::DeserializeOwned>(
    ctx: &TaskContext,
    node_name: &str,
    content: &str,
) -> Result<T, serde_json::Error> {
    let structured = get_result(ctx, node_name).and_then(|value| value.get("structured").cloned());
    match structured {
        Some(value) if !value.is_null() => serde_json::from_value(value),
        _ => serde_json::from_str(strip_json_fence(content)),
    }
}

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
    use super::{default_command_runner, is_noop_commit, strip_json_fence};

    #[test]
    fn bare_json_passes_through_unchanged_but_trimmed() {
        assert_eq!(strip_json_fence("  {\"a\": 1}  "), "{\"a\": 1}");
    }

    #[test]
    fn strips_fence_with_json_language_tag() {
        let text = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn strips_bare_fence_with_no_language_tag() {
        let text = "```\n{\"a\": 1}\n```";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn discards_trailing_prose_after_the_closing_fence() {
        let text = "```json\n{\"a\": 1}\n```\nDone!";
        assert_eq!(strip_json_fence(text), "{\"a\": 1}");
    }

    #[test]
    fn unclosed_fence_falls_back_to_the_whole_trimmed_text() {
        let text = "```json\n{\"a\": 1}";
        assert_eq!(strip_json_fence(text), text);
    }

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
