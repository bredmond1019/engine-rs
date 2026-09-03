#!/usr/bin/env python3
"""Fixture suite for scripts/fleet_build.py.

EN.ticket.fleet-build-permit-wrapper: task 2 wrote this against a DELIBERATE
NO-OP wrapper (per D68 / carryover `gate-scope-must-be-shown-capable-of-
failing`) and recorded it failing red. Task 3 turned the no-op into the real
permit-gated wrapper; `ConcurrencyTest` and `TransparencyTests` are
UNCHANGED in their assertions from task 2 -- only test-isolation setup
(a per-test tmp lock dir, and a memory-override so the real machine's free
RAM can never make the concurrency/transparency tests flaky) was added,
since task 3's wrapper now actually reads FLEET_BUILD_LOCK_DIR and the
memory seams. Task 3 also adds the permit-specific tests: SIGKILL release,
per-dequeue memory re-check, fail-open on an unwritable lock store, and
TTL-based reclaim of a stranded permit.

The concurrency test spawns REAL, SEPARATE subprocesses (not threads, and
no mocked lock) because process-level mutual exclusion is exactly the
property under test -- a single-process test would assert the property into
existence rather than observe it.
"""

from __future__ import annotations

import os
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
WRAPPER = REPO_ROOT / "fleet_build.py"

# A tiny recorder script: appends a "start <epoch>" line, sleeps, then
# appends a "stop <epoch>" line to its own (per-process) log file. Each
# concurrent wrapper invocation gets its OWN log file so there is never a
# multi-writer race on a single file -- the concurrency signal comes from
# comparing timestamps across files afterward, not from shared-file writes.
RECORDER_SRC = """
import sys
import time

path = sys.argv[1]
sleep_s = float(sys.argv[2])

with open(path, "a") as f:
    f.write(f"start {time.time()}\\n")
time.sleep(sleep_s)
with open(path, "a") as f:
    f.write(f"stop {time.time()}\\n")
"""

# A recorder that also writes its OWN pid to a second file as soon as it
# starts, so a test can locate and SIGKILL exactly this process (not the
# wrapper around it) once it is confirmed running.
PID_RECORDER_SRC = """
import os
import sys
import time

pid_path = sys.argv[1]
sleep_s = float(sys.argv[2])

with open(pid_path, "w") as f:
    f.write(str(os.getpid()))
time.sleep(sleep_s)
"""


def _isolated_env(tmp_dir: str, **overrides: str) -> dict:
    """A child-process env with an isolated lock dir and a memory override
    high enough that the real machine's free RAM can never gate a test that
    is not itself testing the memory gate.
    """
    env = dict(os.environ)
    env["FLEET_BUILD_LOCK_DIR"] = str(Path(tmp_dir) / ".fleet-locks" / "builds")
    env.setdefault("FLEET_BUILD_FREE_MB_OVERRIDE", "999999")
    env.update(overrides)
    return env


def max_overlap(intervals: list[tuple[float, float]]) -> int:
    """Return the maximum number of intervals overlapping at any instant."""
    events: list[tuple[float, int]] = []
    for start, stop in intervals:
        events.append((start, 1))
        events.append((stop, -1))
    events.sort()

    current = 0
    best = 0
    for _, delta in events:
        current += delta
        best = max(best, current)
    return best


class TransparencyTests(unittest.TestCase):
    """The wrapper must alter neither the wrapped command's output nor its
    exit code -- these passed against the no-op AND must still pass against
    the real permit-gated wrapper."""

    def test_stdout_and_success_pass_through(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run(
                [sys.executable, str(WRAPPER), "--", "echo", "hi"],
                capture_output=True,
                text=True,
                env=_isolated_env(tmp),
            )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "hi")

    def test_exit_code_passes_through(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            result = subprocess.run(
                [sys.executable, str(WRAPPER), "--", "sh", "-c", "exit 3"],
                env=_isolated_env(tmp),
            )
        self.assertEqual(result.returncode, 3)


class ConcurrencyTest(unittest.TestCase):
    """The load-bearing test: with FLEET_BUILD_MAX=2, six concurrent
    wrapper invocations must never observe more than 2 running at once, and
    all six must complete.

    Passed against the real permit-gated wrapper (task 3); recorded FAILING
    against the task-2 no-op, per the module docstring / completion note.
    """

    def test_max_build_concurrency_is_bounded(self) -> None:
        n = 6
        sleep_s = "0.6"
        fleet_build_max = 2

        with tempfile.TemporaryDirectory() as tmp:
            recorder_path = Path(tmp) / "recorder.py"
            recorder_path.write_text(RECORDER_SRC)

            log_paths = [Path(tmp) / f"{i}.log" for i in range(n)]
            for p in log_paths:
                p.touch()

            env = _isolated_env(tmp, FLEET_BUILD_MAX=str(fleet_build_max))

            procs = [
                subprocess.Popen(
                    [
                        sys.executable,
                        str(WRAPPER),
                        "--",
                        sys.executable,
                        str(recorder_path),
                        str(log_path),
                        sleep_s,
                    ],
                    env=env,
                )
                for log_path in log_paths
            ]

            return_codes = [p.wait() for p in procs]
            self.assertTrue(
                all(rc == 0 for rc in return_codes),
                f"not all {n} wrapper invocations completed cleanly: {return_codes}",
            )

            intervals: list[tuple[float, float]] = []
            for p in log_paths:
                lines = p.read_text().splitlines()
                self.assertEqual(
                    len(lines),
                    2,
                    f"expected exactly a start+stop line in {p}, got: {lines!r}",
                )
                start = float(lines[0].split()[1])
                stop = float(lines[1].split()[1])
                intervals.append((start, stop))

            observed = max_overlap(intervals)
            self.assertLessEqual(
                observed,
                fleet_build_max,
                f"observed concurrency {observed} exceeds FLEET_BUILD_MAX="
                f"{fleet_build_max} across {n} wrapper invocations",
            )


class SigkillReleaseTest(unittest.TestCase):
    """A permit held by a SIGKILLed wrapped command is reclaimed
    immediately -- proven by a following invocation acquiring right away
    rather than waiting out the TTL."""

    def test_sigkilled_command_releases_its_permit_immediately(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            recorder_path = Path(tmp) / "pid_recorder.py"
            recorder_path.write_text(PID_RECORDER_SRC)
            pid_path = Path(tmp) / "pid.txt"

            # Only 1 permit available, and a long TTL -- if release-on-death
            # did not work, a following acquirer would have to wait out the
            # (long) TTL rather than acquiring immediately.
            env = _isolated_env(
                tmp,
                FLEET_BUILD_MAX="1",
                FLEET_BUILD_TTL_SECONDS="3600",
            )

            holder = subprocess.Popen(
                [
                    sys.executable,
                    str(WRAPPER),
                    "--",
                    sys.executable,
                    str(recorder_path),
                    str(pid_path),
                    "30",  # long enough that only a kill ends it
                ],
                env=env,
            )

            # Wait for the wrapped command to actually start and record its
            # own pid (proves the permit was acquired and the child exec'd).
            deadline = time.time() + 10
            while time.time() < deadline and not pid_path.exists():
                time.sleep(0.02)
            self.assertTrue(pid_path.exists(), "wrapped command never started")
            # Small extra wait so the pid write has definitely landed.
            time.sleep(0.1)
            wrapped_pid = int(pid_path.read_text().strip())

            os.kill(wrapped_pid, signal.SIGKILL)
            # holder's subprocess.run(...) will see the child die, hit
            # `finally`, and release the permit -- give it a moment to do so.
            holder.wait(timeout=10)

            # A second invocation should acquire near-instantly, not wait
            # out the 3600s TTL.
            start = time.time()
            result = subprocess.run(
                [sys.executable, str(WRAPPER), "--", "echo", "second"],
                capture_output=True,
                text=True,
                env=env,
                timeout=15,
            )
            elapsed = time.time() - start

            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout.strip(), "second")
            self.assertLess(
                elapsed,
                5.0,
                f"second acquire took {elapsed:.2f}s -- looks like it waited "
                "out the TTL rather than reclaiming the SIGKILLed permit",
            )


class MemoryGateTest(unittest.TestCase):
    """Free-memory admission is re-checked at EACH dequeue attempt, not
    once at enqueue -- an injected low-then-high sequence proves the wait
    ends on the SECOND reading."""

    def test_low_then_high_memory_waits_then_proceeds_on_second_read(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            seq_path = Path(tmp) / "free_mb_sequence.txt"
            # First read: below the 2048MB threshold -> must wait.
            # Second (and subsequent) read: comfortably above -> must admit.
            seq_path.write_text("500\n8000\n")

            env = _isolated_env(tmp)
            env.pop("FLEET_BUILD_FREE_MB_OVERRIDE", None)
            env["FLEET_BUILD_FREE_MB_SEQUENCE_FILE"] = str(seq_path)
            env["FLEET_BUILD_MIN_FREE_MB"] = "2048"
            env["FLEET_BUILD_MAX"] = "2"

            start = time.time()
            result = subprocess.run(
                [sys.executable, str(WRAPPER), "--", "echo", "admitted"],
                capture_output=True,
                text=True,
                env=env,
                timeout=15,
            )
            elapsed = time.time() - start

            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout.strip(), "admitted")
            # It must have actually waited/polled between the low and high
            # reading (poll interval is 0.05s) -- a near-zero elapsed time
            # would mean the low reading never gated admission at all.
            self.assertGreater(
                elapsed,
                0.03,
                "wrapper admitted immediately -- the low first reading did "
                "not gate admission, so this cannot be proving a per-attempt "
                "re-check",
            )
            # Exactly one line must remain unconsumed (the "8000" reading
            # that finally admitted it) -- proves the check read the
            # sequence twice (enqueue-time low, a later dequeue-time high),
            # not merely once at enqueue.
            remaining = seq_path.read_text().strip().splitlines()
            self.assertEqual(
                remaining,
                ["8000"],
                f"expected the sequence file to have consumed exactly the "
                f"first ('500') reading, leaving '8000' behind; got {remaining!r}",
            )


class FailOpenTest(unittest.TestCase):
    """An unwritable lock store must never hard-fail a build -- the wrapper
    runs the command anyway and warns on stderr."""

    def test_unwritable_lock_dir_runs_command_and_warns(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            unwritable_parent = Path(tmp) / "locked"
            unwritable_parent.mkdir()
            os.chmod(unwritable_parent, 0o500)  # read+execute, no write
            lock_dir = unwritable_parent / "builds"

            env = dict(os.environ)
            env["FLEET_BUILD_LOCK_DIR"] = str(lock_dir)
            env.pop("FLEET_LOCK_DIR", None)

            try:
                result = subprocess.run(
                    [sys.executable, str(WRAPPER), "--", "echo", "still-runs"],
                    capture_output=True,
                    text=True,
                    env=env,
                    timeout=15,
                )
            finally:
                os.chmod(unwritable_parent, 0o700)  # allow tempdir cleanup

            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout.strip(), "still-runs")
            self.assertIn("degraded", result.stderr.lower())
            self.assertIn("allowed", result.stderr.lower())


class StrandedPermitTtlTest(unittest.TestCase):
    """A permit whose holder died without releasing (and whose pid has
    since been reused/is unreliable to check) is reclaimed once its TTL
    passes."""

    def test_stranded_permit_is_reclaimed_after_ttl(self) -> None:
        import json

        with tempfile.TemporaryDirectory() as tmp:
            lock_dir = Path(tmp) / ".fleet-locks" / "builds"
            lock_dir.mkdir(parents=True)

            # A stranded permit with a pid that is essentially guaranteed
            # not to be a running process on this machine, and an
            # already-expired started_at -- simulates a holder that died
            # without releasing, past TTL.
            stale_entry = lock_dir / "stale-permit.json"
            stale_entry.write_text(
                json.dumps({"pid": 999999, "started_at": time.time() - 120})
            )

            env = dict(os.environ)
            env["FLEET_BUILD_LOCK_DIR"] = str(lock_dir)
            env.pop("FLEET_LOCK_DIR", None)
            env["FLEET_BUILD_FREE_MB_OVERRIDE"] = "999999"
            env["FLEET_BUILD_MAX"] = "1"
            env["FLEET_BUILD_TTL_SECONDS"] = "60"  # entry is 120s old -> stale

            result = subprocess.run(
                [sys.executable, str(WRAPPER), "--", "echo", "reclaimed"],
                capture_output=True,
                text=True,
                env=env,
                timeout=15,
            )

            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout.strip(), "reclaimed")
            self.assertFalse(
                stale_entry.exists(), "stranded permit was not swept away"
            )


if __name__ == "__main__":
    unittest.main()
