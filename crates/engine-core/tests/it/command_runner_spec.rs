//! Process-lifecycle coverage for the `CommandRunner`/`SpecCommandRunner`
//! seams (`EN.ticket.command-runner-timeout-and-env`).
//!
//! Task 1 recorded the hang: it invoked a `sleep 30` child through
//! `default_command_runner()` — the seam that existed before this block,
//! with no timeout — under a 1-second budget enforced in Rust via an
//! `Instant` deadline around the call, and observed the call block for the
//! full 30s (the test was `#[ignore]`d so that deliberately-red assertion
//! did not block task 1's own per-task gates).
//!
//! Task 2 adds `CommandSpec`/`default_spec_runner`, which gains exactly the
//! timeout `default_command_runner()` lacks. This test now targets that new
//! seam directly, is no longer `#[ignore]`d, and asserts the call returns a
//! typed timeout error well under the budget — the equivalent assertion
//! that failed in task 1, now true.
//!
//! Does not shell out to the `timeout` utility: it does not exist on this
//! macOS shell (repo CLAUDE.md trap 5), so the deadline is `default_spec_runner`'s
//! own, enforced with `Instant`/`try_wait`, never a shelled-out wrapper.

use engine_core::workflows::{default_spec_runner, CommandSpec, CommandTimeout};
use std::path::Path;
use std::time::{Duration, Instant};

/// Before task 2: `default_command_runner()` has no timeout and blocks a
/// `sleep 30` child for its full runtime — recorded as the reproduction in
/// task 1. After task 2: the equivalent call through `CommandSpec`/
/// `default_spec_runner` with a 1-second timeout returns a typed
/// `CommandTimeout` error in well under 2 seconds.
#[test]
fn default_spec_runner_honors_its_timeout_where_default_command_runner_could_not() {
    let runner = default_spec_runner();
    let budget = Duration::from_secs(1);
    let cwd = Path::new(".");
    let spec = CommandSpec {
        program: "sleep",
        args: &["30"],
        cwd,
        env: &[],
        timeout: Some(budget),
    };

    let start = Instant::now();
    let err = runner(&spec).expect_err("a child exceeding its timeout must return an error");
    let elapsed = start.elapsed();

    // This is the assertion that FAILED against `default_command_runner()`
    // in task 1 (elapsed was ~30s there). Against the new seam it must be
    // true: the deadline fires and the call returns promptly.
    assert!(
        elapsed < Duration::from_secs(2),
        "expected default_spec_runner to honor a ~{budget:?} budget, but it took {elapsed:?}"
    );

    assert_eq!(err.kind(), std::io::ErrorKind::TimedOut);
    let timeout = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<CommandTimeout>())
        .expect("timeout error must carry a typed CommandTimeout, never a bare io::Error");
    assert_eq!(timeout.program, "sleep");
    assert!(
        timeout.elapsed < Duration::from_secs(2),
        "the elapsed duration recorded on the typed error should also be under the budget: {:?}",
        timeout.elapsed
    );
    // Never a zero-status success and never a silent empty result: the
    // typed error path is what carries the (empty, for `sleep`) captured
    // output — there is no `CommandOutput` returned on the timeout path.
    assert!(timeout.stdout.is_empty());
}

/// A killed child must be reaped, not left as a zombie. `ps` reporting the
/// pid as gone right after the call returns is the evidence: a zombie
/// (state `Z`, not yet `wait()`-ed on) would still show up.
#[test]
fn default_spec_runner_leaves_no_zombie_after_a_timeout_kill() {
    let runner = default_spec_runner();
    let cwd = Path::new(".");
    let spec = CommandSpec {
        program: "sleep",
        args: &["30"],
        cwd,
        env: &[],
        timeout: Some(Duration::from_millis(300)),
    };

    let err = runner(&spec).expect_err("a hung child must time out, not succeed");
    let timeout = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<CommandTimeout>())
        .expect("timeout error must carry a typed CommandTimeout");
    let pid = timeout.pid;

    let ps = std::process::Command::new("ps")
        .args(["-o", "pid=", "-p", &pid.to_string()])
        .output()
        .expect("ps should run");
    let ps_stdout = String::from_utf8_lossy(&ps.stdout);
    assert!(
        ps_stdout.trim().is_empty(),
        "pid {pid} should no longer exist after kill()+wait(), but `ps` still reports it: {ps_stdout:?}"
    );
}

/// `CommandSpec`'s `env` reaches the child and never mutates this test
/// process's own environment — the seam is per-call scoped, not global.
#[test]
fn default_spec_runner_passes_env_to_the_child_without_mutating_the_parent() {
    let runner = default_spec_runner();
    let cwd = Path::new(".");

    assert!(std::env::var("ENGINE_RS_IT_SPEC_ENV_TEST_VAR").is_err());

    let spec = CommandSpec {
        program: "sh",
        args: &["-c", "echo $ENGINE_RS_IT_SPEC_ENV_TEST_VAR"],
        cwd,
        env: &[("ENGINE_RS_IT_SPEC_ENV_TEST_VAR", "it-spec-env-value")],
        timeout: None,
    };
    let output = runner(&spec).expect("sh -c echo should run normally");
    assert_eq!(output.status, 0);
    assert_eq!(output.stdout.trim(), "it-spec-env-value");

    assert!(
        std::env::var("ENGINE_RS_IT_SPEC_ENV_TEST_VAR").is_err(),
        "CommandSpec::env must be scoped to the child, never the parent process"
    );
}
