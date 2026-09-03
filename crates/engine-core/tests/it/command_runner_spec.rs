//! Process-lifecycle coverage for the additive `CommandSpec` / `SpecCommandRunner` seam
//! (`EN.ticket.command-runner-timeout-and-env`). These tests spawn real short-lived child
//! processes — a stubbed runner would assert nothing about kill, reap, or env inheritance.
//!
//! Task 1 records the reproduction: today's `CommandRunner` seam
//! (`engine_core::workflows::CommandRunner` / `default_command_runner`) has no timeout
//! parameter at all, so a hung child hangs the caller forever. The only way to express "a
//! `sleep 30` child bounded by a 1-second budget" is against the seam this block adds —
//! `CommandSpec` / `default_spec_runner` — which does not exist yet as of this task. Building
//! this test therefore fails to compile, which IS the recorded reproduction (see the task log /
//! `EN.ticket.command-runner-timeout-and-env` task 1 acceptance criteria: "observed hanging (or
//! failing to compile against the not-yet-added CommandSpec)"). Task 2 adds the type this test
//! needs; once it lands, this test starts compiling and is expected to pass, proving the
//! deadline logic works.

use std::path::Path;
use std::time::{Duration, Instant};

use engine_core::workflows::{default_spec_runner, CommandSpec};

/// A `sleep 30` child bounded by a 1-second timeout must be killed and return a typed timeout
/// error in well under the child's own 30-second runtime — never wait for the child to exit on
/// its own.
///
/// Before task 2 lands `CommandSpec`/`default_spec_runner`, this test does not compile at all:
/// that compile failure is today's concrete defect made visible (the existing `CommandRunner`
/// seam has no way to express a per-call budget, so a hung child hangs the caller with no
/// verdict, forever — precisely the failure class this block exists to fix).
#[test]
fn sleep_30_child_with_1s_budget_is_killed_and_returns_typed_timeout_under_2s() {
    let runner = default_spec_runner();
    let cwd = Path::new(".");
    let spec = CommandSpec {
        program: "sleep",
        args: &["30"],
        cwd,
        env: &[],
        timeout: Some(Duration::from_secs(1)),
    };

    let started = Instant::now();
    let result = runner(&spec);
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "expected the timed-out call to return in well under 2s, took {elapsed:?}"
    );
    assert!(
        result.is_err(),
        "expected a typed timeout error, got Ok({result:?})"
    );
}
