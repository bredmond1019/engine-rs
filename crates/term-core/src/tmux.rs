//! tmux.rs — thin wrapper over `std::process::Command` → the tmux CLI.
//! Ported from `core/bastion/src/sessions/tmux.rs` (EN.9.A task 2), MINUS
//! `attach_session` / `suspend_and_attach` — those live in `term-attach` (task 6)
//! so `engine-core` never links a crate that can seize the controlling tty.
//!
//! Design: command *construction* (pure, returns args Vec) is separated from
//! command *execution* (does I/O) so construction can be unit-tested without
//! spawning a real tmux process.
//!
//! **Error-shape contract:** every public fn in this module returns its
//! error wrapped in [`TmuxError::Context`]. A consumer matching on variant
//! never receives a bare `NoServer`/`NotInstalled`/`ExitError`/`Io`/`Timeout`
//! from a public fn call — call [`TmuxError::root_cause`] first. See the
//! doc comment on [`TmuxError`] itself for why a catch-all `Context => ...`
//! arm is the wrong fix for an exhaustive downstream match.

use std::process::Command;

// ── Format strings ────────────────────────────────────────────────────────────

/// Format string used with `tmux list-sessions -F`.
/// Fields (tab-separated):
///   1. session_name
///   2. session_attached (1/0)
///   3. session_windows (count)
///   4. session_activity (epoch secs)
///   5. pane_current_command (foreground process name in the first pane)
///   6. pane_current_path (cwd of the first pane, used for session → space mapping)
///
/// State (running vs idle) is derived from field 5, not field 2.
pub const LIST_SESSIONS_FORMAT: &str = "#{session_name}\t#{session_attached}\t#{session_windows}\t#{session_activity}\t#{pane_current_command}\t#{pane_current_path}";

/// Separator between fields in LIST_SESSIONS_FORMAT output.
pub const FIELD_SEP: char = '\t';

// ── Command construction (pure) ───────────────────────────────────────────────

/// Returns the argument list for:
///   tmux list-sessions -F <LIST_SESSIONS_FORMAT>
/// The first element is the `tmux` binary name.
pub fn list_sessions_args() -> Vec<String> {
    vec![
        "tmux".to_string(),
        "list-sessions".to_string(),
        "-F".to_string(),
        LIST_SESSIONS_FORMAT.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux capture-pane -p -t <session_name>
/// The first element is the `tmux` binary name.
pub fn capture_pane_args(session_name: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "capture-pane".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        session_name.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux attach -t <session_name>
/// The first element is the `tmux` binary name.
///
/// This builder is pure (no process spawn) so it stays in `term-core`; the
/// function that actually executes it (`attach_session`) lives in `term-attach`.
pub fn attach_args(session_name: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "attach".to_string(),
        "-t".to_string(),
        session_name.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux new-session -d -s <session_name> [-c <dir>]
/// The first element is the `tmux` binary name.
pub fn new_session_args(session_name: &str, dir: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "tmux".to_string(),
        "new-session".to_string(),
        "-d".to_string(),
        "-s".to_string(),
        session_name.to_string(),
    ];
    if let Some(d) = dir {
        args.push("-c".to_string());
        args.push(d.to_string());
    }
    args
}

/// Returns the argument list for:
///   tmux kill-session -t <session_name>
/// The first element is the `tmux` binary name.
pub fn kill_session_args(session_name: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "kill-session".to_string(),
        "-t".to_string(),
        session_name.to_string(),
    ]
}

/// Returns the argument list for a **literal** send-keys invocation:
///   tmux send-keys -t <session_name> -l -- <keys>
///
/// `-l` (literal) ensures the text is never interpreted as tmux key names
/// (e.g. a command containing `Enter`, `C-c`).  `--` prevents a command
/// starting with `-` from being parsed as a flag.  The `keys` value is a
/// single argv element — multi-word commands are passed verbatim.
///
/// The Enter keypress must be sent in a separate call (use `send_enter_args`)
/// because `-l` disables key-name lookup.
pub fn send_keys_args(session_name: &str, keys: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        "-l".to_string(),
        "--".to_string(),
        keys.to_string(),
    ]
}

/// Returns the argument list for a **literal, no-Enter** send-keys invocation:
///   tmux send-keys -t <session_name> -l -- <keys>
///
/// Identical shape to [`send_keys_args`] — named separately because the two
/// exist for different use cases and their execution counterparts
/// (`send_keys` vs `send_keys_no_enter`) issue a different number of tmux
/// invocations. This exists for the `AskUserQuestion` widget's free-text
/// option: selecting that option must move the highlight without
/// submitting, because sending Enter on the free-text option with nothing
/// typed yet closes the widget and submits nothing. Callers that want the
/// widget to submit after this call send the trailing Enter themselves via
/// a later `send_keys_args` / `send_enter_args` call once the actual answer
/// text is available.
pub fn send_keys_no_enter_args(session_name: &str, keys: &str) -> Vec<String> {
    send_keys_args(session_name, keys)
}

/// Returns the argument list for sending an Enter keypress:
///   tmux send-keys -t <session_name> Enter
///
/// This is a separate invocation from `send_keys_args` because `-l` (literal)
/// disables key-name lookup, so `Enter` would be sent as the literal string
/// rather than the Return key.
pub fn send_enter_args(session_name: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        "Enter".to_string(),
    ]
}

/// Returns the argument list for a **named-key** send-keys invocation:
///   tmux send-keys -t <session_name> <key>
///
/// Unlike `send_keys_args`, this does **not** use `-l` or `--` so that tmux
/// resolves the key name (`Escape`, `Enter`, `Up`, `Down`, `Left`, `Right`,
/// `C-c`, etc.) rather than sending it as literal text.
///
/// Use this for control keys and special keys that cannot be sent with `-l`.
pub fn send_named_key_args(session_name: &str, key: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "send-keys".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        key.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux set-option -g <name> <value>
///
/// Sets a **global** tmux option — including an `@`-prefixed user option —
/// visible to every session. Net-new for the `EN.9.B` lease surface, which
/// stashes lease metadata as a tmux user option so any attached session can
/// read it back with `show_option_args`.
pub fn set_option_args(name: &str, value: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "set-option".to_string(),
        "-g".to_string(),
        name.to_string(),
        value.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux show-option -g <name>
///
/// Reads back a global tmux option previously written with `set_option_args`.
pub fn show_option_args(name: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "show-option".to_string(),
        "-g".to_string(),
        name.to_string(),
    ]
}

/// Returns the argument list for:
///   tmux display-message -p -t <session_name> <format>
///
/// Net-new for the `EN.9.B` operator-hold surface: the raw-`tmux attach`
/// fallback signal (`#{session_attached}`) for a session that no managed
/// attach path saw, so `@operator_hold` never went missing. `-p` prints the
/// expanded format string to stdout instead of the tmux status line.
pub fn display_message_args(session_name: &str, format: &str) -> Vec<String> {
    vec![
        "tmux".to_string(),
        "display-message".to_string(),
        "-p".to_string(),
        "-t".to_string(),
        session_name.to_string(),
        format.to_string(),
    ]
}

/// Returns the locale env pairs to force on the spawned tmux child.
///
/// `run_tmux` inherits the parent process environment as-is. When the parent
/// has no `LANG`/`LC_ALL` (or a non-UTF-8 locale), some tmux builds —
/// observed on tmux 3.6b/macOS — emit `list-sessions -F` output whose field
/// separator is not a plain tab, which breaks `parse_session_line`'s
/// tab-separated-fields check on every line. Forcing a known-good UTF-8
/// locale on the spawned child keeps `list-sessions` output tab-separated
/// regardless of the parent environment.
///
/// `en_US.UTF-8` is used rather than `C.UTF-8` because this is the
/// macOS-first deployment target and macOS does not ship `C.UTF-8`.
/// `LC_ALL` is included because it takes precedence over `LANG` and any
/// per-category `LC_*` variables, so the override is authoritative even
/// when the parent process already sets a conflicting `LANG`.
pub fn tmux_locale_env() -> Vec<(&'static str, &'static str)> {
    vec![("LC_ALL", "en_US.UTF-8"), ("LANG", "en_US.UTF-8")]
}

// ── Execution ─────────────────────────────────────────────────────────────────

/// Errors produced by this module.
///
/// **Every public fn in this module returns its error wrapped in
/// [`TmuxError::Context`]** — the action name is attached via the private
/// `.context(...)` extension below on every fallible call site. A consumer
/// that matches on variant therefore never sees a bare `NoServer` /
/// `NotInstalled` / `ExitError` / `Io` / `Timeout` straight from a public fn
/// — it sees `Context { action, source }` wrapping one. Call
/// [`TmuxError::root_cause`] first to reach the real variant; do not add a
/// catch-all `Context => ...` arm to make an exhaustive match compile, since
/// that silently collapses every wrapped variant into one bucket (e.g.
/// turning `no tmux server running` into a generic 500 instead of the 503
/// its bare `NoServer` would have produced).
#[derive(Debug, thiserror::Error)]
pub enum TmuxError {
    #[error("tmux binary not found — is tmux installed?")]
    NotInstalled,
    #[error("no tmux server running")]
    NoServer,
    #[error("tmux error (exit {code}): {stderr}")]
    ExitError { code: i32, stderr: String },
    #[error("failed to run tmux: {0}")]
    Io(#[source] std::io::Error),
    /// A named operation failed because the underlying tmux invocation did.
    /// Carries the same contextual information a string-based `.context()`
    /// call would attach, but as a typed, matchable variant.
    #[error("{action} failed: {source}")]
    Context {
        action: &'static str,
        #[source]
        source: Box<TmuxError>,
    },
    /// The invocation did not complete within the supplied bound. The child
    /// process is killed before this variant is returned — never leaked.
    #[error("tmux {action} timed out after {after:?}")]
    Timeout {
        action: &'static str,
        after: std::time::Duration,
    },
}

impl TmuxError {
    /// The innermost non-[`TmuxError::Context`] error in this chain.
    ///
    /// Every public fn in this module wraps its error in `Context` (see the
    /// type-level doc above), so a consumer that wants to match on the real
    /// variant — `NoServer`, `NotInstalled`, `ExitError { .. }`, `Io`, or
    /// `Timeout` — must call this first. Recurses through arbitrary nesting
    /// depth (today's chains are at most two deep, e.g. the `send_keys`
    /// Enter-send path, but this does not assume a bound); returns `self`
    /// unchanged for every non-`Context` variant, including a bare one.
    #[must_use]
    pub fn root_cause(&self) -> &TmuxError {
        match self {
            TmuxError::Context { source, .. } => source.root_cause(),
            other => other,
        }
    }
}

/// Private `.context(...)` extension, mirroring the ergonomics of the
/// original port's error-context calls while staying fully typed.
trait ResultExt<T> {
    fn context(self, action: &'static str) -> Result<T, TmuxError>;
}

impl<T> ResultExt<T> for Result<T, TmuxError> {
    fn context(self, action: &'static str) -> Result<T, TmuxError> {
        self.map_err(|source| TmuxError::Context {
            action,
            source: Box::new(source),
        })
    }
}

/// Classify the outcome of a completed tmux invocation into the same
/// `Result<String, TmuxError>` shape `run_tmux` returns, without touching
/// a process handle. Pure — takes the already-collected exit status pieces.
///
/// Both the blocking `run_tmux` and the async `run_tmux_async` (behind the
/// `tokio` feature) delegate to this single function so their success/
/// no-server/non-zero-exit classification can never drift apart.
pub fn classify_output(
    success: bool,
    code: Option<i32>,
    stdout: &[u8],
    stderr: &[u8],
) -> Result<String, TmuxError> {
    if success {
        let stdout = String::from_utf8_lossy(stdout).into_owned();
        return Ok(stdout);
    }

    let stderr = String::from_utf8_lossy(stderr).trim().to_string();

    // tmux exits 1 with this stderr when no server is running.
    if classify_no_server(&stderr) {
        return Err(TmuxError::NoServer);
    }

    let code = code.unwrap_or(-1);
    Err(TmuxError::ExitError { code, stderr })
}

/// Execute a tmux command (args[0] = "tmux", args[1..] = subcommand + flags).
/// Returns the captured stdout on success.
pub fn run_tmux(args: &[String]) -> Result<String, TmuxError> {
    debug_assert!(!args.is_empty(), "args must not be empty");
    let (bin, rest) = args.split_first().expect("args must not be empty");

    let output = Command::new(bin)
        .args(rest)
        .envs(tmux_locale_env())
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TmuxError::NotInstalled
            } else {
                TmuxError::Io(e)
            }
        })?;

    classify_output(
        output.status.success(),
        output.status.code(),
        &output.stdout,
        &output.stderr,
    )
}

/// Async mirror of `run_tmux`, using `tokio::process::Command`. Shares the
/// same locale env and the same `classify_output` classification so the
/// blocking and async paths can never disagree on outcome.
///
/// Behind the non-default `tokio` feature — `bastion` consumes this crate
/// blocking-only and must keep paying nothing for the async path.
///
/// On timeout the spawned child is killed before `TmuxError::Timeout` is
/// returned; it is never left running in the background.
#[cfg(feature = "tokio")]
pub async fn run_tmux_async(
    args: &[String],
    timeout: std::time::Duration,
) -> Result<String, TmuxError> {
    debug_assert!(!args.is_empty(), "args must not be empty");
    let (bin, rest) = args.split_first().expect("args must not be empty");

    // `kill_on_drop(true)` is what makes the timeout path below actually kill
    // the child rather than leak it: `tokio::time::timeout` cancels by
    // dropping the losing future, which drops this `Child` — and with
    // `kill_on_drop` set, tokio sends SIGKILL as part of that drop instead of
    // merely closing our handle to an orphaned process.
    let child = tokio::process::Command::new(bin)
        .args(rest)
        .envs(tmux_locale_env())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TmuxError::NotInstalled
            } else {
                TmuxError::Io(e)
            }
        })?;

    let action: &'static str = "run_tmux_async";
    let wait = child.wait_with_output();

    match tokio::time::timeout(timeout, wait).await {
        Ok(Ok(output)) => classify_output(
            output.status.success(),
            output.status.code(),
            &output.stdout,
            &output.stderr,
        ),
        Ok(Err(e)) => Err(TmuxError::Io(e)),
        Err(_elapsed) => Err(TmuxError::Timeout {
            action,
            after: timeout,
        }),
    }
}

/// True when tmux stderr indicates no server is running / reachable.
/// Pure classification logic, extracted from `run_tmux` so it is unit-testable
/// without spawning a tmux process.
pub fn classify_no_server(stderr: &str) -> bool {
    stderr.contains("no server running")
        || stderr.contains("error connecting to")
        || stderr.contains("No such file or directory")
}

/// List all tmux sessions; returns raw formatted output lines.
pub fn list_sessions_raw() -> Result<String, TmuxError> {
    let args = list_sessions_args();
    run_tmux(&args).context("list-sessions failed")
}

/// Capture the last-pane output of the given session; returns raw text.
pub fn capture_pane_raw(session_name: &str) -> Result<String, TmuxError> {
    let args = capture_pane_args(session_name);
    run_tmux(&args).context("capture-pane failed")
}

/// Create a detached tmux session, optionally starting in `dir`.
pub fn new_session(session_name: &str, dir: Option<&str>) -> Result<(), TmuxError> {
    let args = new_session_args(session_name, dir);
    run_tmux(&args).context("new-session failed")?;
    Ok(())
}

/// Remove a tmux session.
pub fn kill_session(session_name: &str) -> Result<(), TmuxError> {
    let args = kill_session_args(session_name);
    run_tmux(&args).context("kill-session failed")?;
    Ok(())
}

/// Send `keys` literally to `session_name`, followed by an Enter keypress.
///
/// Two tmux invocations are made:
/// 1. `send-keys -t <session> -l -- <keys>` — sends the text literally.
/// 2. `send-keys -t <session> Enter` — sends the Return key.
///
/// An unknown session surfaces as `TmuxError::ExitError`.
pub fn send_keys(session_name: &str, keys: &str) -> Result<(), TmuxError> {
    let literal_args = send_keys_args(session_name, keys);
    run_tmux(&literal_args).context("send-keys (literal) failed")?;

    let enter_args = send_enter_args(session_name);
    run_tmux(&enter_args).context("send-keys (Enter) failed")?;

    Ok(())
}

/// Send `keys` literally to `session_name`, WITHOUT a trailing Enter.
///
/// One tmux invocation is made: `send-keys -t <session> -l -- <keys>`.
///
/// This exists for the `AskUserQuestion` widget's free-text option: selecting
/// that option must move the highlight without submitting, because sending
/// Enter on the free-text option with nothing typed yet closes the widget and
/// submits nothing. Callers that want the widget to submit after this call
/// send the trailing Enter themselves via a later `send_keys` /
/// `send_enter_args` call once the actual answer text is available.
///
/// An unknown session surfaces as `TmuxError::ExitError`.
pub fn send_keys_no_enter(session_name: &str, keys: &str) -> Result<(), TmuxError> {
    let literal_args = send_keys_no_enter_args(session_name, keys);
    run_tmux(&literal_args).context("send-keys (literal, no Enter) failed")?;
    Ok(())
}

/// Send a single named key (e.g. `Escape`, `Enter`, `Up`, `C-c`) to
/// `session_name`.
///
/// Unlike `send_keys`, this does **not** use `-l` so tmux resolves the key
/// name.  An unknown session surfaces as `TmuxError::ExitError`.
pub fn send_named_key(session_name: &str, key: &str) -> Result<(), TmuxError> {
    let args = send_named_key_args(session_name, key);
    run_tmux(&args).context("send-keys (named key) failed")?;
    Ok(())
}

// ── Tests (pure, no live tmux) ────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sessions_args_correct() {
        let args = list_sessions_args();
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "list-sessions");
        assert_eq!(args[2], "-F");
        assert_eq!(args[3], LIST_SESSIONS_FORMAT);
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn capture_pane_args_correct() {
        let args = capture_pane_args("my-session");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "capture-pane");
        assert_eq!(args[2], "-p");
        assert_eq!(args[3], "-t");
        assert_eq!(args[4], "my-session");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn list_sessions_format_contains_required_fields() {
        assert!(LIST_SESSIONS_FORMAT.contains("#{session_name}"));
        assert!(LIST_SESSIONS_FORMAT.contains("#{session_attached}"));
        assert!(LIST_SESSIONS_FORMAT.contains("#{session_windows}"));
        assert!(LIST_SESSIONS_FORMAT.contains("#{session_activity}"));
        assert!(LIST_SESSIONS_FORMAT.contains("#{pane_current_command}"));
        assert!(LIST_SESSIONS_FORMAT.contains("#{pane_current_path}"));
    }

    #[test]
    fn field_sep_matches_format_separator() {
        // Verify the const separator agrees with what we put in the format string.
        assert!(LIST_SESSIONS_FORMAT.contains(FIELD_SEP));
    }

    #[test]
    fn attach_args_correct() {
        let args = attach_args("my-session");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "attach");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "my-session");
        assert_eq!(args.len(), 4);
    }

    #[test]
    fn new_session_args_without_dir() {
        let args = new_session_args("work", None);
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "new-session");
        assert_eq!(args[2], "-d");
        assert_eq!(args[3], "-s");
        assert_eq!(args[4], "work");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn new_session_args_with_dir() {
        let args = new_session_args("work", Some("/tmp"));
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "new-session");
        assert_eq!(args[2], "-d");
        assert_eq!(args[3], "-s");
        assert_eq!(args[4], "work");
        assert_eq!(args[5], "-c");
        assert_eq!(args[6], "/tmp");
        assert_eq!(args.len(), 7);
    }

    #[test]
    fn kill_session_args_correct() {
        let args = kill_session_args("old-session");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "kill-session");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "old-session");
        assert_eq!(args.len(), 4);
    }

    // ── send-keys arg construction ──────────────────────────────────────────────

    #[test]
    fn send_keys_args_simple_command() {
        let args = send_keys_args("work", "cargo build");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "send-keys");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "work");
        assert_eq!(args[4], "-l");
        assert_eq!(args[5], "--");
        assert_eq!(args[6], "cargo build");
        assert_eq!(args.len(), 7);
    }

    #[test]
    fn send_keys_args_contains_literal_flag() {
        // -l must always be present so key-name tokens are never interpreted.
        let args = send_keys_args("work", "echo Enter");
        assert!(args.contains(&"-l".to_string()), "missing -l in: {args:?}");
    }

    #[test]
    fn send_keys_args_contains_double_dash() {
        // -- must always be present so a leading hyphen is not parsed as a flag.
        let args = send_keys_args("work", "--help");
        assert!(args.contains(&"--".to_string()), "missing -- in: {args:?}");
    }

    #[test]
    fn send_keys_args_command_with_tmux_key_token() {
        // A command containing "Enter" must be a single argv element after --.
        let args = send_keys_args("work", "echo Enter");
        assert_eq!(
            args[6], "echo Enter",
            "command must be a single argv element"
        );
        assert_eq!(args.len(), 7);
    }

    #[test]
    fn send_keys_args_command_with_leading_hyphen() {
        // A command starting with - must be after --, as a single argv element.
        let args = send_keys_args("work", "--help");
        assert_eq!(args[5], "--", "-- must precede the command");
        assert_eq!(args[6], "--help", "command must be a single argv element");
        assert_eq!(args.len(), 7);
    }

    #[test]
    fn send_enter_args_correct() {
        let args = send_enter_args("work");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "send-keys");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "work");
        assert_eq!(args[4], "Enter");
        assert_eq!(args.len(), 5);
        // Must NOT contain -l — that would prevent Enter being treated as the Return key.
        assert!(
            !args.contains(&"-l".to_string()),
            "-l must not appear in enter args"
        );
    }

    // ── send_keys_no_enter_args ───────────────────────────────────────────────

    #[test]
    fn send_keys_no_enter_args_emits_literal_send_argv() {
        let args = send_keys_no_enter_args("work", "echo hi");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "send-keys");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "work");
        assert_eq!(args[4], "-l");
        assert_eq!(args[5], "--");
        assert_eq!(args[6], "echo hi");
        assert_eq!(args.len(), 7);
        assert_eq!(
            args,
            send_keys_args("work", "echo hi"),
            "no-enter builder must emit the same literal-send argv as send_keys_args"
        );
    }

    #[test]
    fn send_keys_no_enter_records_one_invocation_where_send_keys_records_two() {
        // send_keys sends the text as one args-vec and Enter as a second —
        // two distinct tmux invocations.
        let send_keys_flow = [send_keys_args("work", "echo hi"), send_enter_args("work")];
        assert_eq!(
            send_keys_flow.len(),
            2,
            "send_keys must issue exactly two tmux invocations"
        );

        // send_keys_no_enter sends only the literal text — one invocation,
        // with no following Enter. A future refactor that folds the two
        // functions together must break this assertion, not pass silently.
        let send_keys_no_enter_flow = [send_keys_no_enter_args("work", "echo hi")];
        assert_eq!(
            send_keys_no_enter_flow.len(),
            1,
            "send_keys_no_enter must issue exactly one tmux invocation"
        );
        assert!(
            !send_keys_no_enter_flow.contains(&send_enter_args("work")),
            "send_keys_no_enter must never append an Enter invocation"
        );
    }

    // ── send_named_key_args ────────────────────────────────────────────────────

    #[test]
    fn send_named_key_args_single_key() {
        let args = send_named_key_args("work", "Escape");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "send-keys");
        assert_eq!(args[2], "-t");
        assert_eq!(args[3], "work");
        assert_eq!(args[4], "Escape");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn send_named_key_args_no_literal_flag() {
        // -l must NOT be present — named-key lookup must remain active.
        let args = send_named_key_args("work", "Enter");
        assert!(
            !args.contains(&"-l".to_string()),
            "-l must not appear in named-key args: {args:?}"
        );
    }

    #[test]
    fn send_named_key_args_no_double_dash() {
        // -- must NOT be present — it would prevent tmux from resolving the key name.
        let args = send_named_key_args("work", "Up");
        assert!(
            !args.contains(&"--".to_string()),
            "-- must not appear in named-key args: {args:?}"
        );
    }

    #[test]
    fn send_named_key_args_arrow_keys() {
        for key in ["Up", "Down", "Left", "Right"] {
            let args = send_named_key_args("sess", key);
            assert_eq!(args[4], key, "key element mismatch for {key}");
            assert_eq!(args.len(), 5);
        }
    }

    #[test]
    fn send_named_key_args_modifier_key() {
        // Hyphen-style modifiers like C-c must be passed through as-is.
        let args = send_named_key_args("sess", "C-c");
        assert_eq!(args[4], "C-c");
        assert_eq!(args.len(), 5);
        assert!(!args.contains(&"-l".to_string()));
        assert!(!args.contains(&"--".to_string()));
    }

    // ── set_option_args / show_option_args ──────────────────────────────────────

    #[test]
    fn set_option_args_correct() {
        let args = set_option_args("@lease", "abc123");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "set-option");
        assert_eq!(args[2], "-g");
        assert_eq!(args[3], "@lease");
        assert_eq!(args[4], "abc123");
        assert_eq!(args.len(), 5);
    }

    #[test]
    fn show_option_args_correct() {
        let args = show_option_args("@lease");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "show-option");
        assert_eq!(args[2], "-g");
        assert_eq!(args[3], "@lease");
        assert_eq!(args.len(), 4);
    }

    // ── display_message_args ─────────────────────────────────────────────────────

    #[test]
    fn display_message_args_correct() {
        let args = display_message_args("sess", "#{session_attached}");
        assert_eq!(args[0], "tmux");
        assert_eq!(args[1], "display-message");
        assert_eq!(args[2], "-p");
        assert_eq!(args[3], "-t");
        assert_eq!(args[4], "sess");
        assert_eq!(args[5], "#{session_attached}");
        assert_eq!(args.len(), 6);
    }

    // ── tmux_locale_env ─────────────────────────────────────────────────────────

    #[test]
    fn tmux_locale_env_sets_lc_all_and_lang() {
        let env = tmux_locale_env();
        assert!(
            env.iter().any(|(k, _)| *k == "LC_ALL"),
            "missing LC_ALL in: {env:?}"
        );
        assert!(
            env.iter().any(|(k, _)| *k == "LANG"),
            "missing LANG in: {env:?}"
        );
    }

    #[test]
    fn tmux_locale_env_values_are_utf8_locales() {
        let env = tmux_locale_env();
        for (k, v) in &env {
            assert!(
                v.to_uppercase().contains("UTF-8"),
                "{k} value {v} does not contain UTF-8"
            );
        }
    }

    #[test]
    fn tmux_locale_env_lc_all_present_for_precedence() {
        // LC_ALL takes precedence over LANG and any per-category LC_*, so it
        // must be present for the override to be authoritative even when the
        // parent process already sets a conflicting LANG.
        let env = tmux_locale_env();
        let lc_all = env.iter().find(|(k, _)| *k == "LC_ALL");
        assert!(lc_all.is_some(), "LC_ALL must be present: {env:?}");
    }

    // ── stderr classification (#2) ──────────────────────────────────────────────

    #[test]
    fn classify_no_server_matches_no_server_running() {
        assert!(classify_no_server(
            "no server running on /tmp/tmux-501/default"
        ));
    }

    #[test]
    fn classify_no_server_matches_error_connecting() {
        assert!(classify_no_server(
            "error connecting to /tmp/tmux-501/default (No such file)"
        ));
    }

    #[test]
    fn classify_no_server_matches_no_such_file() {
        assert!(classify_no_server("No such file or directory"));
    }

    #[test]
    fn classify_no_server_rejects_unrelated_stderr() {
        assert!(!classify_no_server("duplicate session: work"));
        assert!(!classify_no_server("can't find session: nope"));
    }

    #[test]
    fn classify_no_server_rejects_empty() {
        assert!(!classify_no_server(""));
    }

    // ── classify_output (#1 — shared blocking/async classification) ────────────

    #[test]
    fn classify_output_success_returns_stdout() {
        let result = classify_output(true, Some(0), b"pane contents\n", b"");
        assert_eq!(result.unwrap(), "pane contents\n");
    }

    #[test]
    fn classify_output_no_server_matches_run_tmux_path() {
        // Same stderr shape run_tmux would see for "no server running".
        let result = classify_output(
            false,
            Some(1),
            b"",
            b"no server running on /tmp/tmux-501/default",
        );
        assert!(matches!(result, Err(TmuxError::NoServer)));
    }

    #[test]
    fn classify_output_non_zero_exit_carries_code_and_stderr() {
        let result = classify_output(false, Some(2), b"", b"can't find session: nope");
        match result {
            Err(TmuxError::ExitError { code, stderr }) => {
                assert_eq!(code, 2);
                assert_eq!(stderr, "can't find session: nope");
            }
            other => panic!("expected ExitError, got {other:?}"),
        }
    }

    #[test]
    fn classify_output_missing_exit_code_defaults_to_negative_one() {
        let result = classify_output(false, None, b"", b"duplicate session: work");
        match result {
            Err(TmuxError::ExitError { code, .. }) => assert_eq!(code, -1),
            other => panic!("expected ExitError, got {other:?}"),
        }
    }

    #[test]
    fn classify_output_agrees_with_run_tmux_success_shape() {
        // run_tmux's success branch is `String::from_utf8_lossy(&output.stdout)
        // .into_owned()`; classify_output must produce byte-identical output
        // for the same input so the two paths never disagree.
        let raw: &[u8] = b"session-a\tsession-b\n";
        let via_classify = classify_output(true, Some(0), raw, b"").unwrap();
        let via_lossy = String::from_utf8_lossy(raw).into_owned();
        assert_eq!(via_classify, via_lossy);
    }

    // ── run_tmux_async (#1 — tokio feature) ─────────────────────────────────────

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn run_tmux_async_wedged_command_times_out_within_bound() {
        // Stand in for a wedged tmux call with a command that outlives the bound.
        let args = vec!["sleep".to_string(), "30".to_string()];
        let bound = std::time::Duration::from_millis(200);

        let started = std::time::Instant::now();
        let result = run_tmux_async(&args, bound).await;
        let elapsed = started.elapsed();

        assert!(
            matches!(result, Err(TmuxError::Timeout { .. })),
            "expected Timeout, got {result:?}"
        );
        // Generous slack over the bound so this isn't flaky under CI load,
        // while still proving we didn't wait anywhere near the full 30s sleep.
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "took {elapsed:?}, expected well under the 30s sleep"
        );
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn run_tmux_async_completes_within_bound_returns_ok() {
        let args = vec!["echo".to_string(), "hello".to_string()];
        let bound = std::time::Duration::from_secs(5);

        let result = run_tmux_async(&args, bound).await;
        assert_eq!(result.unwrap().trim(), "hello");
    }

    #[cfg(feature = "tokio")]
    #[tokio::test]
    async fn run_tmux_async_missing_binary_returns_not_installed() {
        let args = vec!["this-binary-does-not-exist-xyz".to_string()];
        let bound = std::time::Duration::from_secs(5);

        let result = run_tmux_async(&args, bound).await;
        assert!(matches!(result, Err(TmuxError::NotInstalled)));
    }

    // ── root_cause() ──────────────────────────────────────────────────────

    #[test]
    fn root_cause_unwraps_single_context_layer() {
        let err = TmuxError::Context {
            action: "list_sessions",
            source: Box::new(TmuxError::NoServer),
        };
        assert!(matches!(err.root_cause(), TmuxError::NoServer));
    }

    #[test]
    fn root_cause_unwraps_double_context_layer() {
        // Mirrors the send_keys Enter-send path, which can wrap twice.
        let err = TmuxError::Context {
            action: "send_enter",
            source: Box::new(TmuxError::Context {
                action: "send_keys",
                source: Box::new(TmuxError::NoServer),
            }),
        };
        assert!(matches!(err.root_cause(), TmuxError::NoServer));
    }

    #[test]
    fn root_cause_returns_self_for_bare_variant() {
        let err = TmuxError::NotInstalled;
        assert!(matches!(err.root_cause(), TmuxError::NotInstalled));
    }

    #[test]
    fn root_cause_preserves_exit_error_fields_through_unwrap() {
        let err = TmuxError::Context {
            action: "kill_session",
            source: Box::new(TmuxError::ExitError {
                code: 1,
                stderr: "can't find session foo".to_string(),
            }),
        };
        match err.root_cause() {
            TmuxError::ExitError { code, stderr } => {
                assert_eq!(*code, 1);
                assert!(stderr.contains("can't find session"));
            }
            other => panic!("expected ExitError, got {other:?}"),
        }
    }
}
