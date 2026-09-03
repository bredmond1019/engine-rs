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
use std::time::Duration;

use claude_code_rs::{Config, Outcome};
use engine_contract::TaskContext;
use futures::future::BoxFuture;

use crate::policy::command_floor::{self, CommandDecision};
use crate::sessions::{self, ClaudeSession};

pub mod approve_and_run;
pub mod claim_reaffirm;
pub mod content_pipeline;
pub mod deliverable_render;
pub mod diagnostic_intake;
pub mod harvest_approve;
pub mod lead_ingest;
pub mod linkedin_post;
pub mod opportunity_edit;
pub mod orchestration;
pub mod proposal_generator;
pub mod recall;
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

/// Snapshot the current length of `ctx`'s session ledger, to be paired with [`sessions_since`]
/// after a wrapper's inner `ClaudeCodeStep` call returns.
///
/// # Why a length, not a clone
///
/// A wrapper that bills the API via an inner step and then fails at parse/validation time
/// currently constructs a bare `NodeError::new(..)`, whose `sessions` is empty — the billed entry
/// dies with the discarded `ctx` when `node_context`'s `Node::process(ctx)` (by-value) call
/// returns `Err`. `node_context` already has a carry-on-error channel (`NodeError::sessions` /
/// `NodeError::with_sessions`, replayed onto the pre-call ledger on the `Err` branch) — the gap is
/// only that these wrappers never populate it for a post-billed-call failure.
///
/// Pairing a cheap `usize` baseline with [`sessions_since`] — rather than diffing two full ledger
/// clones — keeps this a per-invocation delta, not a per-dispatch whole-context clone. Attaching
/// the *whole* ledger onto `NodeError` would double-count every entry already present in the
/// pre-call snapshot `node_context` replays onto (it prepends `err.sessions`, it does not merge
/// them), inflating the run's reported spend — the opposite of what this exists to fix.
pub(crate) fn session_baseline(ctx: &TaskContext) -> usize {
    sessions::read_sessions(&ctx.metadata).len()
}

/// The ledger entries appended to `ctx` after `baseline`, in order.
///
/// Returns an empty vec when nothing was appended, and also when the ledger is shorter than
/// `baseline` (should not happen — the ledger is append-only — but a telemetry helper must never
/// panic or slice out of bounds over a malformed/shrunk ledger; [`sessions::read_sessions`]
/// follows the same never-panic contract for the same reason).
pub(crate) fn sessions_since(ctx: &TaskContext, baseline: usize) -> Vec<ClaudeSession> {
    let all = sessions::read_sessions(&ctx.metadata);
    if all.len() <= baseline {
        return Vec::new();
    }
    all[baseline..].to_vec()
}

#[cfg(test)]
mod session_delta_tests {
    use super::{session_baseline, sessions_since};
    use crate::sessions::{append_session, ClaudeSession};
    use engine_contract::TaskContext;

    fn make_session(node: &str) -> ClaudeSession {
        ClaudeSession {
            node: node.to_string(),
            session_id: Some(format!("sess-{node}")),
            ok: true,
            cost_usd: 0.01,
            input_tokens: 10,
            output_tokens: 5,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            model: String::new(),
            started_at: None,
        }
    }

    fn ctx_with_metadata(metadata: serde_json::Value) -> TaskContext {
        TaskContext {
            event: serde_json::json!({}),
            nodes: Default::default(),
            metadata,
            node_runs: Default::default(),
        }
    }

    #[test]
    fn baseline_on_empty_ledger_is_zero() {
        let ctx = ctx_with_metadata(serde_json::json!({}));
        assert_eq!(session_baseline(&ctx), 0);
    }

    #[test]
    fn sessions_since_returns_only_appended_entries_in_order() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        let mut ctx = ctx_with_metadata(metadata);
        let baseline = session_baseline(&ctx);
        assert_eq!(baseline, 1);

        append_session(&mut ctx.metadata, make_session("b"));
        append_session(&mut ctx.metadata, make_session("c"));

        let delta = sessions_since(&ctx, baseline);
        assert_eq!(delta.len(), 2);
        assert_eq!(delta[0].node, "b");
        assert_eq!(delta[1].node, "c");
    }

    #[test]
    fn sessions_since_empty_when_nothing_appended() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        let ctx = ctx_with_metadata(metadata);
        let baseline = session_baseline(&ctx);

        let delta = sessions_since(&ctx, baseline);
        assert!(delta.is_empty());
    }

    #[test]
    fn sessions_since_never_panics_on_shrunk_ledger() {
        let mut metadata = serde_json::json!({});
        append_session(&mut metadata, make_session("a"));
        append_session(&mut metadata, make_session("b"));
        let ctx = ctx_with_metadata(metadata);

        // Baseline claims a length longer than the ledger actually has.
        let delta = sessions_since(&ctx, 99);
        assert!(delta.is_empty());
    }

    #[test]
    fn sessions_since_on_absent_metadata_is_empty() {
        let ctx = ctx_with_metadata(serde_json::json!(null));
        assert_eq!(session_baseline(&ctx), 0);
        assert!(sessions_since(&ctx, 0).is_empty());
    }
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

/// The default [`CommandRunner`]: a thin delegation onto
/// [`default_spec_runner`] with an empty `env` (inherit-only) and
/// `timeout: None` (wait forever) — today's exact behavior. This keeps
/// [`CommandRunner`]'s signature and every one of its 28 call sites
/// untouched (`EN.ticket.command-runner-timeout-and-env`); the org-floor
/// denylist evaluation and the crate's one `std::process::Command` spawn
/// both live in [`default_spec_runner`] now, not here.
#[must_use]
pub fn default_command_runner() -> CommandRunner {
    let spec_runner = default_spec_runner();
    Arc::new(move |program, args, cwd| {
        let spec = CommandSpec {
            program,
            args,
            cwd,
            env: &[],
            timeout: None,
        };
        spec_runner(&spec)
    })
}

/// Superset description of a subprocess invocation for [`SpecCommandRunner`]
/// — additive to the plain `(program, args, cwd)` triple [`CommandRunner`]
/// takes, carrying per-call environment variables and an optional hard
/// timeout. An empty `env` and `timeout: None` reproduce
/// [`default_command_runner`]'s exact behavior.
#[derive(Debug, Clone, Copy)]
pub struct CommandSpec<'a> {
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub cwd: &'a Path,
    /// Extra environment variables the child sees on top of whatever it
    /// would otherwise inherit from this process. Empty means "inherit
    /// only" — today's behavior. These never mutate this process's own
    /// environment; they are passed straight to `std::process::Command`.
    pub env: &'a [(&'a str, &'a str)],
    /// Hard wall-clock budget for the child. `None` waits forever (today's
    /// behavior). `Some(d)` kills and reaps the child if it has not exited
    /// within `d`, returning a [`CommandTimeout`] error rather than a
    /// success or an empty result.
    pub timeout: Option<Duration>,
}

/// The injectable command-runner signature for callers that need per-call
/// environment variables and/or a hard timeout — the superset seam new
/// subprocess callers (`typst`, `yt-dlp`, `uv run`, Playwright drivers,
/// ...) should reach for instead of a raw `std::process::Command`. Defaults
/// to the real subprocess via [`default_spec_runner`]; tests substitute a
/// stub exactly as they do for [`CommandRunner`].
pub type SpecCommandRunner =
    Arc<dyn Fn(&CommandSpec) -> std::io::Result<CommandOutput> + Send + Sync>;

/// Typed error returned when a [`SpecCommandRunner`] child exceeds its
/// [`CommandSpec::timeout`]. Carried as the source of an
/// `io::Error(ErrorKind::TimedOut, ..)` so the seam's return type stays
/// `std::io::Result<CommandOutput>` — downcast via
/// `err.get_ref().and_then(|e| e.downcast_ref::<CommandTimeout>())` to
/// recover the typed detail. Never a zero-status success and never a
/// silent empty result: whatever stdout/stderr the child had produced
/// before the kill is preserved here.
#[derive(Debug)]
pub struct CommandTimeout {
    pub program: String,
    pub elapsed: Duration,
    pub stdout: String,
    pub stderr: String,
    /// The killed child's pid, for diagnostics/tests that want to confirm
    /// the process is actually gone (not left as a zombie).
    pub pid: u32,
}

impl std::fmt::Display for CommandTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "command `{}` (pid {}) timed out after {:?}",
            self.program, self.pid, self.elapsed
        )
    }
}

impl std::error::Error for CommandTimeout {}

/// The default [`SpecCommandRunner`]: shells out to the real subprocess via
/// `std::process::Command` — gated by the non-overridable
/// [`command_floor::evaluate_command`] org-floor denylist, exactly as
/// [`default_command_runner`] used to do directly. This is now the ONLY
/// place in the crate that evaluates the floor and spawns a child; a
/// denied command never reaches `std::process::Command`.
///
/// Timeout semantics: the child's stdout/stderr are drained on background
/// threads (so a chatty child can't deadlock on a full pipe buffer while
/// this polls), and `try_wait` is polled against a deadline. On expiry the
/// child is `kill()`-ed and then `wait()`-ed to reap it — never left as a
/// zombie — and the call returns a [`CommandTimeout`] naming the program,
/// pid, elapsed duration, and whatever output had been captured so far.
#[must_use]
pub fn default_spec_runner() -> SpecCommandRunner {
    Arc::new(|spec: &CommandSpec| {
        let joined = std::iter::once(spec.program)
            .chain(spec.args.iter().copied())
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

        let mut child = std::process::Command::new(spec.program)
            .args(spec.args)
            .current_dir(spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (*k, *v)))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()?;
        let pid = child.id();

        // Drain stdout/stderr concurrently so a child that writes more
        // than a pipe buffer's worth of output can't deadlock the poll
        // loop below (which never reads the pipes itself).
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();
        let stdout_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            }
            buf
        });
        let stderr_handle = std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = stderr_pipe {
                let _ = std::io::Read::read_to_end(&mut pipe, &mut buf);
            }
            buf
        });

        let start = std::time::Instant::now();
        let exit_status = loop {
            if let Some(status) = child.try_wait()? {
                break Some(status);
            }
            if let Some(timeout) = spec.timeout {
                if start.elapsed() >= timeout {
                    break None;
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        };

        let collect = |handle: std::thread::JoinHandle<Vec<u8>>| -> String {
            String::from_utf8_lossy(&handle.join().unwrap_or_default()).into_owned()
        };

        match exit_status {
            Some(status) => Ok(CommandOutput {
                status: status.code().unwrap_or(-1),
                stdout: collect(stdout_handle),
                stderr: collect(stderr_handle),
            }),
            None => {
                // Deadline hit: kill then wait() to reap — a kill() alone
                // leaves a zombie until someone waits on the pid.
                let _ = child.kill();
                let _ = child.wait();
                let elapsed = start.elapsed();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    CommandTimeout {
                        program: spec.program.to_string(),
                        elapsed,
                        stdout: collect(stdout_handle),
                        stderr: collect(stderr_handle),
                        pid,
                    },
                ))
            }
        }
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
/// Returns a [`CommitOutcome`] rather than a bare `bool`: a `false` used to
/// collapse the ordinary "nothing to commit" no-op into the same value as a
/// genuine git failure, so no caller could gate on the difference. A
/// [`CommitOutcome::Failed`] means `HEAD` did **not** advance while there was
/// real work to record — which silently breaks the topology invariant for the
/// next task (its `git diff HEAD` would then include this task's work too) —
/// and callers that record a unit of work as done must refuse to do so on it.
/// [`CommitOutcome::NoOp`] is the benign case and must stay benign.
///
/// The classification lives in [`is_noop_commit`], the single place that
/// decides which of the two a non-zero `git commit` exit is; never
/// re-implement its string matching at a call site.
pub(crate) fn commit_all(runner: &CommandRunner, worktree: &Path, message: &str) -> CommitOutcome {
    let _ = runner("git", &["add", "-A"], worktree);
    let commit = runner("git", &["commit", "-m", message], worktree);
    match &commit {
        Ok(output) if output.status == 0 => CommitOutcome::Committed,
        Ok(output) => {
            // "nothing to commit" or an equivalent no-op — logged, not
            // an error, mirroring `save_state_node.py`. A genuine failure
            // is logged too, and additionally handed back to the caller.
            log_noop_commit(message, output);
            if is_noop_commit(&output.stderr, &output.stdout) {
                CommitOutcome::NoOp
            } else {
                CommitOutcome::Failed {
                    detail: output.stderr.trim().to_string(),
                }
            }
        }
        Err(err) => CommitOutcome::Failed {
            detail: format!("git commit could not be run: {err}"),
        },
    }
}

/// The three distinguishable outcomes of a [`commit_all`] call.
///
/// Split out of the former `bool` because the two `false` cases mean opposite
/// things: `NoOp` is the routine "nothing to commit, working tree clean"
/// (every state commit in this repo, where `planning/` is a gitignored
/// symlink) and must not fail anything, while `Failed` is a real git error
/// whose work was never recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CommitOutcome {
    /// `git commit` exited 0 — `HEAD` advanced.
    Committed,
    /// `git commit` exited non-zero with a "nothing to commit" style message,
    /// per [`is_noop_commit`]. Benign.
    NoOp,
    /// `git commit` genuinely failed; `detail` carries the git stderr.
    Failed { detail: String },
}

impl CommitOutcome {
    /// `true` only when `HEAD` actually advanced.
    pub(crate) fn is_committed(&self) -> bool {
        matches!(self, CommitOutcome::Committed)
    }

    /// The git error text when this is a genuine failure, else `None` — the
    /// accessor callers gate on.
    pub(crate) fn failure_detail(&self) -> Option<&str> {
        match self {
            CommitOutcome::Failed { detail } => Some(detail.as_str()),
            _ => None,
        }
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
    use super::{
        commit_all, default_command_runner, default_spec_runner, is_noop_commit, strip_json_fence,
        CommandOutput, CommandRunner, CommandSpec, CommitOutcome,
    };
    use std::sync::Arc;

    /// A runner whose `git commit` returns the given exit code/stderr and
    /// whose every other invocation succeeds.
    fn commit_runner(status: i32, stderr: &'static str) -> CommandRunner {
        Arc::new(move |_program, args: &[&str], _cwd| {
            if args.first() == Some(&"commit") {
                Ok(CommandOutput {
                    status,
                    stdout: String::new(),
                    stderr: stderr.to_string(),
                })
            } else {
                Ok(CommandOutput {
                    status: 0,
                    stdout: String::new(),
                    stderr: String::new(),
                })
            }
        })
    }

    #[test]
    fn commit_all_reports_a_successful_commit_as_committed() {
        let outcome = commit_all(&commit_runner(0, ""), std::path::Path::new("."), "chore: x");
        assert_eq!(outcome, CommitOutcome::Committed);
        assert!(outcome.is_committed());
        assert_eq!(outcome.failure_detail(), None);
    }

    #[test]
    fn commit_all_reports_nothing_to_commit_as_a_noop_not_a_failure() {
        let outcome = commit_all(
            &commit_runner(1, "nothing to commit, working tree clean"),
            std::path::Path::new("."),
            "chore: x",
        );
        assert_eq!(outcome, CommitOutcome::NoOp);
        assert!(!outcome.is_committed());
        assert_eq!(
            outcome.failure_detail(),
            None,
            "a no-op must never present as a failure — that distinction is the point"
        );
    }

    #[test]
    fn commit_all_reports_a_genuine_git_error_as_a_failure_carrying_its_stderr() {
        let outcome = commit_all(
            &commit_runner(1, "fatal: unable to write new index file"),
            std::path::Path::new("."),
            "chore: x",
        );
        assert!(!outcome.is_committed());
        assert_eq!(
            outcome.failure_detail(),
            Some("fatal: unable to write new index file")
        );
    }

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
    fn command_spec_with_empty_env_and_no_timeout_matches_default_command_runner() {
        let tmp = std::env::temp_dir();
        let spec_runner = default_spec_runner();
        let spec = CommandSpec {
            program: "echo",
            args: &["hi"],
            cwd: &tmp,
            env: &[],
            timeout: None,
        };
        let spec_output = spec_runner(&spec).expect("echo via CommandSpec should run normally");

        let plain_runner = default_command_runner();
        let plain_output = plain_runner("echo", &["hi"], &tmp)
            .expect("echo via CommandRunner should run normally");

        assert_eq!(spec_output.status, plain_output.status);
        assert_eq!(spec_output.stdout, plain_output.stdout);
        assert_eq!(spec_output.stderr, plain_output.stderr);
    }

    #[test]
    fn default_spec_runner_blocks_a_denied_command_without_spawning() {
        let runner = default_spec_runner();
        let bogus_cwd = std::path::Path::new("/no/such/directory/for/this/test");
        let spec = CommandSpec {
            program: "git",
            args: &["push", "--force"],
            cwd: bogus_cwd,
            env: &[],
            timeout: None,
        };
        let output =
            runner(&spec).expect("denied command must short-circuit before spawning, not error");
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
    fn default_spec_runner_passes_env_to_the_child_without_mutating_the_parent() {
        let tmp = std::env::temp_dir();
        let runner = default_spec_runner();

        // The parent process must never see this var, before or after.
        assert!(std::env::var("ENGINE_RS_SPEC_ENV_TEST_VAR").is_err());

        let spec = CommandSpec {
            program: "sh",
            args: &["-c", "echo $ENGINE_RS_SPEC_ENV_TEST_VAR"],
            cwd: &tmp,
            env: &[("ENGINE_RS_SPEC_ENV_TEST_VAR", "spec-env-value")],
            timeout: None,
        };
        let output = runner(&spec).expect("sh -c echo should run normally");
        assert_eq!(output.status, 0);
        assert_eq!(output.stdout.trim(), "spec-env-value");

        assert!(
            std::env::var("ENGINE_RS_SPEC_ENV_TEST_VAR").is_err(),
            "CommandSpec::env must be scoped to the child, never the parent process"
        );
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
