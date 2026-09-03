//! Process-lifecycle coverage for the `CommandRunner` seam
//! (`EN.ticket.command-runner-timeout-and-env`).
//!
//! Task 1 records the hang: it invokes a `sleep 30` child through
//! `default_command_runner()` — the seam that exists TODAY, with no timeout
//! — under a 1-second budget enforced in Rust via an `Instant` deadline
//! around the call. That budget cannot be honored by the existing seam
//! because `default_command_runner()` blocks on `Command::output()` until
//! the child exits; there is nothing to poll early. The test is marked
//! `#[ignore]` so this deliberately-red assertion does not block this
//! task's own per-task gates — task 2 removes the `#[ignore]`, repoints it
//! at the new `CommandSpec`/`default_spec_runner` seam, and turns it green.
//!
//! Deliberately does NOT import `CommandSpec` or `default_spec_runner` —
//! those don't exist until task 2, and a test that fails to COMPILE is not
//! a red test, it is a broken build (this is the exact defect that sank the
//! prior attempt at this task, commit `9276650`, reverted in `dbd8237`).
//!
//! Does not shell out to the `timeout` utility: it does not exist on this
//! macOS shell (repo CLAUDE.md trap 5), so the deadline is ours in Rust.

use engine_core::workflows::default_command_runner;
use std::path::Path;
use std::time::{Duration, Instant};

/// Reproduction: `default_command_runner()` has no timeout, so a hung child
/// blocks the call for its full runtime. Run with `--ignored` to observe
/// the failure directly; run without it (the default) and this test is
/// skipped, so the rest of the workspace suite passes untouched.
#[test]
#[ignore = "deliberately red until task 2 adds CommandSpec/default_spec_runner's timeout; un-ignored and repointed there"]
fn default_command_runner_has_no_timeout_and_blocks_past_the_budget() {
    let runner = default_command_runner();
    let budget = Duration::from_secs(1);
    let start = Instant::now();

    // `default_command_runner` blocks on `Command::output()` — there is no
    // way to enforce `budget` against it from the caller side, which is
    // exactly the defect this block exists to fix. The call is left
    // un-guarded (no thread/timeout wrapper) so the observed wall-clock
    // time below is the seam's own behavior, not a harness artifact.
    let result = runner("sleep", &["30"], Path::new("."));
    let elapsed = start.elapsed();

    // Reproduction assertion: today's seam does NOT return within the
    // budget. This is expected to FAIL right now (elapsed will be ~30s,
    // not <2s) — that failure IS the reproduction. Task 2 makes the
    // equivalent assertion true against the new seam.
    assert!(
        result.is_ok(),
        "sleep 30 should still exit cleanly once (eventually) reaped: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "expected default_command_runner to honor a ~{budget:?} budget, but it blocked for \
         {elapsed:?} — this is the hang this block exists to fix (task 2 adds the timeout)"
    );
}
