//! `term-attach` — the terminal-attach path, deliberately isolated from `term-core`.
//!
//! This crate holds exactly two functions: [`attach_session`] and [`suspend_and_attach`].
//! Both hand the calling process's controlling terminal to `tmux attach` and block until the
//! user detaches (`Ctrl-b d`).
//!
//! **This crate is depended on by bastion's CLI only.** `engine-core` and `engine-serve` must
//! never add it as a dependency. A feature flag on `term-core` cannot substitute for this split:
//! Cargo features unify **additively**, so if `attach_session` lived behind a feature on
//! `term-core`, the single build of `bastion` (which pulls in both the blocking CLI path and,
//! through `engine-serve -> engine-core -> term-core`, the tokio server path) would compile one
//! rlib with the feature enabled for *both* callers — making `attach_session` reachable and
//! `pub` from inside the engine process, which has no controlling tty and nothing sane to attach
//! to. A crate `engine-core` never links is the only mechanism that actually prevents that call
//! from compiling on that path. Do not "simplify" this back into a feature.

use std::process::{Command, Stdio};

use term_core::tmux::{attach_args, tmux_locale_env, TmuxError};

/// Attach to an existing tmux session, handing the terminal to tmux.
/// Blocks until the user detaches (Ctrl-b d), then returns control.
///
/// On failure (e.g. the session does not exist), the error carries tmux's **actual** stderr
/// output and exit code — never a fabricated message. tmux's own wording ("can't find session:
/// ...", or whatever it says for that failure mode) is what gets surfaced.
pub fn attach_session(session_name: &str) -> Result<(), TmuxError> {
    let args = attach_args(session_name);
    debug_assert!(!args.is_empty(), "args must not be empty");
    let (bin, rest) = args.split_first().expect("args must not be empty");

    let child = Command::new(bin)
        .args(rest)
        .envs(tmux_locale_env())
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                TmuxError::NotInstalled
            } else {
                TmuxError::ExitError {
                    code: -1,
                    stderr: format!("failed to run tmux: {e}"),
                }
            }
        })?;

    let output = child.wait_with_output().map_err(|e| TmuxError::ExitError {
        code: -1,
        stderr: format!("failed to wait on tmux: {e}"),
    })?;

    if output.status.success() {
        return Ok(());
    }

    let code = output.status.code().unwrap_or(-1);
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(TmuxError::ExitError { code, stderr })
}

/// Attach to an existing tmux session, handing the terminal to tmux.
/// Before attaching, prints a styled banner instructing the user how to detach.
pub fn suspend_and_attach(session_name: &str) -> Result<(), TmuxError> {
    // Clear screen and print banner.
    // Use ANSI escape codes for clearing screen and bold styled text.
    print!("\x1B[2J\x1B[1;1H"); // clear screen and move cursor to top left
    println!(
        "\x1B[1m[ BASTION ]\x1B[0m Attaching to Agent. Press \x1B[1mCtrl-b d\x1B[0m to detach and return.\n"
    );
    use std::io::Write;
    std::io::stdout().flush().ok();

    attach_session(session_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Builds a fake `tmux` executable in a temp dir that always fails, writing
    /// `stderr_text` to stderr and exiting with `exit_code`. Returns the temp dir (kept alive
    /// for the caller) and the `PATH` value with that dir prepended.
    fn fake_tmux_path(stderr_text: &str, exit_code: i32) -> (tempfile::TempDir, String) {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("tmux");
        let script = format!("#!/bin/sh\necho '{stderr_text}' 1>&2\nexit {exit_code}\n");
        fs::write(&script_path, script).expect("write fake tmux");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), existing_path);
        (dir, new_path)
    }

    #[test]
    fn attach_session_surfaces_real_tmux_stderr_not_fabricated() {
        let expected_stderr = "session not found: totally-real-tmux-message";
        let (_dir, path) = fake_tmux_path(expected_stderr, 1);

        let original_path = std::env::var("PATH").ok();
        // SAFETY: this test does not run concurrently with other PATH-sensitive tests in this
        // process (nextest forks one process per test).
        unsafe {
            std::env::set_var("PATH", &path);
        }

        let result = attach_session("nonexistent-session");

        if let Some(p) = original_path {
            unsafe {
                std::env::set_var("PATH", p);
            }
        }

        let err = result.expect_err("expected attach_session to fail");
        match err {
            TmuxError::ExitError { code, stderr } => {
                assert_eq!(code, 1);
                assert_eq!(stderr, expected_stderr);
                // The old bastion behaviour fabricated this exact message regardless of what
                // tmux actually said — assert it is gone.
                assert_ne!(stderr, "can't find session: nonexistent-session");
            }
            other => panic!("expected ExitError, got {other:?}"),
        }
    }

    #[test]
    fn attach_session_succeeds_when_tmux_exits_zero() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script_path = dir.path().join("tmux");
        fs::write(&script_path, "#!/bin/sh\nexit 0\n").expect("write fake tmux");
        let mut perms = fs::metadata(&script_path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms).expect("chmod");

        let existing_path = std::env::var("PATH").unwrap_or_default();
        let new_path = format!("{}:{}", dir.path().display(), existing_path);
        let original_path = std::env::var("PATH").ok();
        unsafe {
            std::env::set_var("PATH", &new_path);
        }

        let result = attach_session("some-session");

        if let Some(p) = original_path {
            unsafe {
                std::env::set_var("PATH", p);
            }
        }

        assert!(result.is_ok());
    }

    #[test]
    fn attach_session_not_installed_when_tmux_missing() {
        let original_path = std::env::var("PATH").ok();
        // An empty PATH guarantees `tmux` cannot be found.
        unsafe {
            std::env::set_var("PATH", "");
        }

        let result = attach_session("whatever");

        if let Some(p) = original_path {
            unsafe {
                std::env::set_var("PATH", p);
            }
        }

        assert!(matches!(result, Err(TmuxError::NotInstalled)));
    }
}
