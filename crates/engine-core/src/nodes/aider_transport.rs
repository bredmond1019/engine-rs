//! `AiderTransport` (`EN.16.C` task 2) — the `AgentBackend::Aider` implementation
//! of the existing [`MetaTransport`] alias. No new trait or type alias, same
//! as [`super::pi_transport`] (the reference implementation this module
//! mirrors closely): this only builds a closure of the shape
//! `AgentCodeStep`/`ImplementTaskNode` already accept (`with_meta_transport`).
//!
//! It shells to `aider --model ollama_chat/<local.model> --yes-always
//! --no-show-model-warnings --no-attribute-co-authored-by --message <prompt>`,
//! with `OLLAMA_API_BASE` (aider's own env var — distinct from `pi`'s
//! `OLLAMA_HOST`) set from `local.endpoint`. Confirmed against the operator's
//! real capture,
//! `planning/open-work/pre-plan/pluggable-code-agent-transport/evidence/aider-real-cli-run.md`
//! in the HQ vault (plain-text reference only — never read at runtime; see
//! the parser section below).
//!
//! **DECISION: `--no-attribute-co-authored-by`.** `aider` appends a
//! `Co-authored-by: aider (<model>) <aider@aider.chat>` trailer to every
//! commit it makes by default. This fleet's own convention forbids
//! `Co-Authored-By` trailers on commits it authors directly — that rule is
//! about *this* fleet's own commits, and an `aider` dispatch commits into
//! the *target* repo's worktree, not this one, so the rule does not
//! technically reach it. This transport suppresses the trailer anyway, for
//! consistency: a commit this fleet's own tooling caused should not carry an
//! attribution trailer this fleet otherwise refuses to write, regardless of
//! which repo it lands in.
//!
//! `Config.cwd` (the scoped git worktree) becomes the child's `current_dir`;
//! `Config.timeout` (default [`DEFAULT_AIDER_TIMEOUT`], sized identically to
//! `pi_transport`'s — a shared local Ollama server can queue concurrent
//! lanes) wraps the whole call in one [`tokio::time::timeout`]. An optional
//! [`CancellationToken`] is raced against the same call. Either path KILLS
//! the child — `kill_on_drop(true)` reaps it when the owning future is
//! dropped — and the returned error's text says which of the two occurred.
//! This is the exact same pattern `pi_transport::run_pi` uses; nothing new
//! was invented here.
//!
//! # SAFETY BOUNDARY
//!
//! Identical in kind to `pi_transport`'s, restated here because this module
//! is read independently: `--yes-always` bypasses per-tool approval, and the
//! worktree cwd is **NOT** containment — a subprocess's `current_dir` only
//! sets where relative paths resolve, so the model can write anywhere the
//! operator's user can, use their git/gh credentials, and reach the network.
//! **Never point this backend at an untrusted task description or a
//! cloud-hosted model without revisiting this.**
//!
//! # The plain-text parser
//!
//! Unlike `pi`'s `--mode json` event stream, `aider` has no structured
//! output mode for scripted use — its real output (embedded below as the
//! test fixture; see `evidence/aider-real-cli-run-raw.txt` in the HQ vault
//! for the original capture) is plain text. This parser scans stdout for
//! `Applied edit to <file>` lines (the edited files, in order) and a
//! `Commit <sha> <message>` line, present only when `aider` auto-committed
//! (it is aider's default behavior for an applied edit — absent when it
//! edited but did not commit, or made no edit at all). There is no
//! equivalent of `pi`'s malformed-stream rejection: plain text has no
//! well-formedness to fail, so this parser is infallible.
//!
//! Cost is the outer `None` — not `Some(CostEstimate { dollars: None, .. })`
//! like `pi`'s shape — per this block's AC9: `aider`'s plain-text output
//! carries a `Tokens: N sent, M received` line, but this transport does not
//! parse it into a cost estimate at all, so downstream cost-honesty code
//! sees "no cost information was ever offered" rather than "a cost estimate
//! exists whose dollar figure happens to be unknown".

use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use claude_code_rs::{Config, Error as ClaudeError, Outcome, Result as ClaudeResult};
use futures::future::BoxFuture;
use tokio::process::Command;

use crate::cancellation::CancellationToken;
use crate::policy::LocalConfig;

use super::agent_code_step::{MetaTransport, TransportInfo};
use super::agent_outcome::{translate, AgentOutcome};

/// The `aider` binary name resolved on `PATH`, absent an override (see
/// [`AIDER_BINARY_ENV`]).
const AIDER_BINARY: &str = "aider";

/// Env var that overrides which `aider` binary to spawn — mirrors
/// `pi_transport::PI_BINARY_ENV`. Tests use this to substitute a fake script
/// instead of a real `aider` install.
const AIDER_BINARY_ENV: &str = "AIDER_BINARY";

/// Whole-call wall-clock ceiling applied when `Config.timeout` carries no
/// override — same value and same rationale as `pi_transport::DEFAULT_PI_TIMEOUT`
/// (a shared local Ollama server can queue concurrent lanes).
const DEFAULT_AIDER_TIMEOUT: Duration = Duration::from_secs(600);

/// Build a [`MetaTransport`] that drives `aider` against `local`'s resolved
/// model instead of the `claude` CLI, honoring `cancellation` if supplied (in
/// addition to whatever cancellation the calling node's own
/// `with_cancellation_token` races the whole future against — this transport
/// checks it too so a cancellation is distinguishable, in the returned
/// error's text, from a timeout).
#[must_use]
pub fn aider_meta_transport(
    local: LocalConfig,
    cancellation: Option<CancellationToken>,
) -> MetaTransport {
    Arc::new(move |config: Config, prompt: String| {
        let local = local.clone();
        let cancellation = cancellation.clone();
        Box::pin(run_aider(config, prompt, local, cancellation))
            as BoxFuture<'static, ClaudeResult<(Outcome, TransportInfo)>>
    })
}

/// Convenience: [`aider_meta_transport`] with no cancellation token, for a
/// caller that only needs the composed node's own `with_cancellation_token`
/// race (whose drop-on-cancel still kills the child via `kill_on_drop`, just
/// without this transport's own distinguishing error text).
#[must_use]
pub fn aider_meta_transport_live(local: LocalConfig) -> MetaTransport {
    aider_meta_transport(local, None)
}

/// What the whole-call race resolved to, before the `Output` (or lack of
/// one) is turned into a `claude_code_rs::Error`. Identical shape to
/// `pi_transport::RaceOutcome`.
enum RaceOutcome {
    Completed(std::io::Result<std::process::Output>),
    Cancelled,
}

async fn run_aider(
    config: Config,
    prompt: String,
    local: LocalConfig,
    cancellation: Option<CancellationToken>,
) -> ClaudeResult<(Outcome, TransportInfo)> {
    let binary = std::env::var(AIDER_BINARY_ENV).unwrap_or_else(|_| AIDER_BINARY.to_string());

    let mut command = Command::new(&binary);
    command
        .arg("--model")
        .arg(format!("ollama_chat/{}", local.model))
        .arg("--yes-always")
        .arg("--no-show-model-warnings")
        .arg("--no-attribute-co-authored-by")
        .arg("--message")
        .arg(&prompt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    if let Some(cwd) = &config.cwd {
        command.current_dir(cwd);
    }

    // `aider`'s own env var for the `ollama_chat/` provider base URL —
    // distinct from `pi`'s `OLLAMA_HOST` (see module doc).
    command.env("OLLAMA_API_BASE", &local.endpoint);

    if !config.env.is_empty() {
        command.envs(config.env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(missing_binary_error(&binary));
        }
        Err(err) => return Err(ClaudeError::Spawn(err)),
    };

    let call = child.wait_with_output();
    let timeout_duration = config.timeout.unwrap_or(DEFAULT_AIDER_TIMEOUT);

    let raced = tokio::time::timeout(timeout_duration, async {
        match &cancellation {
            Some(token) => {
                tokio::select! {
                    () = token.cancelled() => RaceOutcome::Cancelled,
                    result = call => RaceOutcome::Completed(result),
                }
            }
            None => RaceOutcome::Completed(call.await),
        }
    })
    .await;

    let output = match raced {
        Err(_elapsed) => return Err(timeout_error(timeout_duration)),
        Ok(RaceOutcome::Cancelled) => return Err(cancelled_error()),
        Ok(RaceOutcome::Completed(io_result)) => io_result.map_err(ClaudeError::Spawn)?,
    };

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let exit_code = output.status.code();

    let parsed = parse_aider_output(&stdout);
    let agent_outcome = build_agent_outcome(parsed, exit_code, &stdout, &stderr);

    Ok(translate(agent_outcome, "aider"))
}

/// `Error::Spawn` naming the binary and its install command, for a spawn
/// that failed because `binary` was not found — never a spawn panic.
fn missing_binary_error(binary: &str) -> ClaudeError {
    ClaudeError::Spawn(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!(
            "`{binary}` not found on PATH. Install aider: \
             `uv tool install --python 3.12 aider-chat` (a bare `uv tool install \
             aider-chat` with no --python pin can resolve to a Python with no \
             prebuilt scipy wheel and fail building it from source — see \
             https://aider.chat/docs/install.html)."
        ),
    ))
}

/// The call exceeded `timeout` — the child is killed by `kill_on_drop` when
/// the timed-out future (which owns it) is dropped.
fn timeout_error(timeout: Duration) -> ClaudeError {
    ClaudeError::Cli {
        status: None,
        stderr: format!(
            "aider call timed out after {timeout:?} waiting on the child process \
             (a shared local Ollama server can queue concurrent lanes) — child killed"
        ),
    }
}

/// A `CancellationToken` won the race — the child is killed by `kill_on_drop`
/// when the dropped future (which owns it) is dropped.
fn cancelled_error() -> ClaudeError {
    ClaudeError::Cli {
        status: None,
        stderr: "aider call cancelled via CancellationToken before completion — child killed"
            .to_string(),
    }
}

// -- plain-text output parsing --

/// What one parsed `aider` plain-text run reported: the files it applied an
/// edit to (in the order the `Applied edit to <file>` lines appeared), and
/// the auto-commit it made, when it made one at all.
#[derive(Debug, Default, PartialEq, Eq)]
struct ParsedAiderOutput {
    /// Files named on an `Applied edit to <file>` line.
    modified_files: Vec<String>,
    /// `(sha, message)` from a `Commit <sha> <message>` line, when the run
    /// auto-committed. Absent when `aider` edited but did not commit
    /// (`--no-auto-commits`, not used by this transport, or nothing to
    /// commit) or made no edit at all.
    commit: Option<(String, String)>,
}

/// Scan `stdout` for `Applied edit to <file>` and `Commit <sha> <message>`
/// lines. Infallible: plain text has no well-formedness to reject, unlike
/// `pi_transport::parse_pi_stream`'s JSON-lines stream.
fn parse_aider_output(stdout: &str) -> ParsedAiderOutput {
    let mut modified_files = Vec::new();
    let mut commit = None;

    for raw_line in stdout.lines() {
        let line = raw_line.trim();
        if let Some(file) = line.strip_prefix("Applied edit to ") {
            modified_files.push(file.trim().to_string());
        } else if let Some(rest) = line.strip_prefix("Commit ") {
            if let Some((sha, message)) = rest.split_once(' ') {
                commit = Some((sha.to_string(), message.to_string()));
            }
        }
    }

    ParsedAiderOutput {
        modified_files,
        commit,
    }
}

/// Turn a parsed plain-text run plus the process's own exit status into the
/// [`AgentOutcome`] `translate` maps onto `(Outcome, TransportInfo)`.
///
/// Success is judged by the process's own exit code, matching
/// `pi_transport::build_agent_outcome`'s own rule. `cost` is the outer
/// `None` unconditionally — see the module doc's parser section.
fn build_agent_outcome(
    parsed: ParsedAiderOutput,
    exit_code: Option<i32>,
    stdout: &str,
    stderr: &str,
) -> AgentOutcome {
    let success = exit_code == Some(0);
    let text = if success {
        stdout.trim().to_string()
    } else {
        format!(
            "aider exited with status {exit_code:?}. stderr: {}",
            stderr.trim()
        )
    };

    AgentOutcome {
        success,
        text,
        modified_files: parsed.modified_files,
        cost: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;

    // Every test in this module mutates the process-global `AIDER_BINARY`
    // env var. `cargo nextest run` (standing rule 8) forks one process per
    // test, so this lock only guards against a stray plain `cargo test` run
    // — matches `pi_transport::PI_BINARY_ENV_LOCK`.
    static AIDER_BINARY_ENV_LOCK: Mutex<()> = Mutex::new(());

    fn test_local_config() -> LocalConfig {
        LocalConfig {
            endpoint: "http://localhost:11434".to_string(),
            model: "test-model".to_string(),
            constrained_json: false,
        }
    }

    #[cfg(unix)]
    fn write_fake_binary(body: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("temp dir");
        let script_path = dir.path().join("fake-aider.sh");
        let mut file = std::fs::File::create(&script_path).expect("create script");
        writeln!(file, "#!/bin/sh\n{body}").expect("write script");
        drop(file);

        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x");

        (dir, script_path)
    }

    #[cfg(unix)]
    fn set_aider_binary(path: &std::path::Path) {
        // SAFETY: single-threaded within this test's own process (nextest
        // forks one process per test), scoped to this test.
        unsafe {
            std::env::set_var(AIDER_BINARY_ENV, path);
        }
    }

    #[cfg(unix)]
    fn clear_aider_binary() {
        // SAFETY: see `set_aider_binary`.
        unsafe {
            std::env::remove_var(AIDER_BINARY_ENV);
        }
    }

    // -- missing binary --

    #[tokio::test]
    async fn missing_binary_produces_error_naming_binary_and_install_command() {
        let _guard = AIDER_BINARY_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // SAFETY: scoped to this test, serialized via the lock above.
        unsafe {
            std::env::set_var(AIDER_BINARY_ENV, "/definitely/not/a/real/aider/binary/xyz");
        }

        let transport = aider_meta_transport(test_local_config(), None);
        let result = transport(Config::default(), "hello".to_string()).await;

        unsafe {
            std::env::remove_var(AIDER_BINARY_ENV);
        }

        let err = result.expect_err("missing binary must error, not panic");
        let message = err.to_string();
        assert!(
            message.contains("/definitely/not/a/real/aider/binary/xyz"),
            "error must name the binary that was missing: {message}"
        );
        assert!(
            message.contains("install"),
            "error must name the install command: {message}"
        );
    }

    // -- plain-text parser --

    /// The operator's real `aider` capture, verbatim, from
    /// `evidence/aider-real-cli-run-raw.txt` (HQ vault,
    /// `install-aider-and-capture-a-real-cli-run` operator session,
    /// 2026-09-12) — embedded as a local constant per this task's own
    /// instruction not to `include_str!` a vaulted path at runtime.
    const REAL_CAPTURE_RAW_TEXT: &str = r#"Warning: Input is not a terminal (fd=0).
────────────────────────────────────────────────────────────────────────────────
You can skip this check with --no-gitignore
Added .aider* to .gitignore
Aider v0.86.2
Model: ollama_chat/qwen2.5:3b with whole edit format
Git repo: .git with 1 files
Repo-map: using 4096.0 tokens, auto refresh


https://aider.chat/HISTORY.html#release-notes

Sure, I will create the hello.txt file with your specified content. Here is the
file:

hello.txt


hello from aider


Please add this file to the chat so we can proceed further if needed.

Tokens: 790 sent, 47 received.

hello.txt
Applied edit to hello.txt
Commit 00fd2a1 feat: add hello.txt with content "hello from aider"
"#;

    #[test]
    fn parser_reads_the_operators_real_capture_text() {
        let parsed = parse_aider_output(REAL_CAPTURE_RAW_TEXT);

        assert_eq!(
            parsed.modified_files,
            vec!["hello.txt".to_string()],
            "must extract the file named on the 'Applied edit to' line of the \
             operator's real aider-real-cli-run-raw.txt capture"
        );
        let (sha, message) = parsed
            .commit
            .expect("real capture's auto-commit line must parse");
        assert_eq!(sha, "00fd2a1");
        assert_eq!(
            message,
            "feat: add hello.txt with content \"hello from aider\""
        );
    }

    #[test]
    fn parser_reports_no_commit_when_stdout_carries_no_commit_line() {
        let parsed = parse_aider_output("Applied edit to notes.md\n");

        assert_eq!(parsed.modified_files, vec!["notes.md".to_string()]);
        assert!(
            parsed.commit.is_none(),
            "no 'Commit ...' line must parse to no commit, not a fabricated one"
        );
    }

    #[test]
    fn parser_reports_no_files_and_no_commit_on_a_no_op_run() {
        let parsed = parse_aider_output("Sure, nothing to change here.\n");

        assert!(parsed.modified_files.is_empty());
        assert!(parsed.commit.is_none());
    }

    // -- build_agent_outcome: cost is the outer None, always --

    #[test]
    fn build_agent_outcome_success_reports_modified_files_and_no_cost() {
        let parsed = ParsedAiderOutput {
            modified_files: vec!["hello.txt".to_string()],
            commit: Some(("00fd2a1".to_string(), "feat: add hello.txt".to_string())),
        };

        let outcome = build_agent_outcome(parsed, Some(0), "some stdout", "");

        assert!(outcome.success);
        assert_eq!(outcome.modified_files, vec!["hello.txt".to_string()]);
        assert!(
            outcome.cost.is_none(),
            "an aider outcome's cost must be the outer None entirely, not \
             Some(CostEstimate {{ dollars: None, .. }})"
        );
    }

    #[test]
    fn build_agent_outcome_failure_on_nonzero_exit_reports_stderr() {
        let parsed = ParsedAiderOutput::default();

        let outcome = build_agent_outcome(parsed, Some(1), "", "boom");

        assert!(!outcome.success);
        assert!(outcome.text.contains("boom"));
        assert!(outcome.cost.is_none());
    }

    // -- translate(): backend name and cost_known --

    #[test]
    fn aider_outcome_translates_to_backend_aider_and_cost_known_false() {
        let outcome = AgentOutcome {
            success: true,
            text: "did the thing".to_string(),
            modified_files: vec!["hello.txt".to_string()],
            cost: None,
        };

        let (claude_outcome, info) = translate(outcome, "aider");

        assert_eq!(info.backend, "aider");
        assert!(!info.cost_known);
        assert_eq!(claude_outcome.cost_usd, 0.0);
    }

    // -- subprocess lifecycle: cwd, timeout-kill, cancellation-kill --

    #[cfg(unix)]
    #[tokio::test]
    async fn subprocess_runs_with_configured_cwd_as_current_dir() {
        let _guard = AIDER_BINARY_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let (_script_dir, script) = write_fake_binary(
            r#"printf 'cwd_is:%s\n' "$(pwd)"
printf 'Applied edit to hello.txt\n'
"#,
        );
        set_aider_binary(&script);

        let cwd_dir = tempfile::tempdir().expect("cwd temp dir");
        let expected_cwd = cwd_dir.path().canonicalize().expect("canonicalize cwd");

        let config = Config {
            cwd: Some(expected_cwd.clone()),
            ..Config::default()
        };

        let transport = aider_meta_transport(test_local_config(), None);
        let result = transport(config, "prompt".to_string()).await;
        clear_aider_binary();

        let (outcome, _info) = result.expect("fake aider script run must succeed");
        assert!(
            outcome.text.contains(&expected_cwd.display().to_string()),
            "child must have run with the configured cwd: {} not found in {}",
            expected_cwd.display(),
            outcome.text
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_kills_the_child_process() {
        let _guard = AIDER_BINARY_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let marker_dir = tempfile::tempdir().expect("marker temp dir");
        let started = marker_dir.path().join("started");
        let done = marker_dir.path().join("done");

        // Writes `started` immediately, sleeps well past the configured
        // timeout, then writes `done` — a marker that must never appear if
        // the child was actually killed rather than left to run to
        // completion in the background. Mirrors
        // `pi_transport::timeout_kills_the_child_process` exactly.
        let (_script_dir, script) = write_fake_binary(
            r#"touch "$STARTED"
sleep 3
touch "$DONE"
"#,
        );
        set_aider_binary(&script);

        let config = Config {
            timeout: Some(Duration::from_millis(300)),
            env: vec![
                ("STARTED".to_string(), started.display().to_string()),
                ("DONE".to_string(), done.display().to_string()),
            ],
            ..Config::default()
        };

        let transport = aider_meta_transport(test_local_config(), None);
        let result = transport(config, "prompt".to_string()).await;
        clear_aider_binary();

        let err = result.expect_err("a 300ms timeout against a 3s sleep must time out");
        assert!(
            err.to_string().contains("timed out"),
            "failure text must say a timeout occurred: {err}"
        );

        wait_for_marker(&started, "child never started");
        // Give the un-killed case every chance to prove itself: wait well
        // past the 3s sleep the script would need to reach `touch "$DONE"`.
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !done.exists(),
            "child was not actually killed — it ran to completion past the timeout"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancellation_kills_the_child_process() {
        let _guard = AIDER_BINARY_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        let marker_dir = tempfile::tempdir().expect("marker temp dir");
        let started = marker_dir.path().join("started");
        let done = marker_dir.path().join("done");

        let (_script_dir, script) = write_fake_binary(
            r#"touch "$STARTED"
sleep 3
touch "$DONE"
"#,
        );
        set_aider_binary(&script);

        let token = CancellationToken::new();
        let config = Config {
            env: vec![
                ("STARTED".to_string(), started.display().to_string()),
                ("DONE".to_string(), done.display().to_string()),
            ],
            ..Config::default()
        };

        let transport = aider_meta_transport(test_local_config(), Some(token.clone()));
        let call = transport(config, "prompt".to_string());

        let cancel_token = token.clone();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            cancel_token.cancel();
        });

        let result = call.await;
        canceller.await.expect("canceller task must not panic");
        clear_aider_binary();

        let err = result.expect_err("a cancelled call must error, not succeed");
        assert!(
            err.to_string().contains("cancel"),
            "failure text must say a cancellation occurred: {err}"
        );

        wait_for_marker(&started, "child never started");
        std::thread::sleep(Duration::from_secs(3));
        assert!(
            !done.exists(),
            "child was not actually killed — it ran to completion past the cancellation"
        );
    }

    #[cfg(unix)]
    fn wait_for_marker(marker: &std::path::Path, panic_message: &str) {
        // 750 * 20ms = 15s, matching `pi_transport`'s own sibling helper —
        // widened from 1s under a full `--workspace --all-features` run
        // competing for CPU against thousands of other tests. See also this
        // test's nextest `retries` override in `.config/nextest.toml`
        // (added by `EN.16.C` task 5 for the integration-test analogue of
        // this same fake-child/marker-file pattern).
        for _ in 0..750 {
            if marker.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("{panic_message}: {} was never created", marker.display());
    }
}
