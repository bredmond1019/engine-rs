#!/usr/bin/env python3
"""Fixture suite for scripts/fleet_build.py.

EN.ticket.fleet-build-permit-wrapper, task 2: written and run against the
DELIBERATE NO-OP wrapper first (per D68 / carryover
`gate-scope-must-be-shown-capable-of-failing`). The concurrency test below
is the load-bearing one and is expected to FAIL against the no-op --
observing it fail here is the point: a concurrency assertion that has never
gone red may simply be watching a machine that was never busy. Task 3 turns
the no-op into the real permit-gated wrapper and this same test must then
go green with no changes to its assertions.

The concurrency test spawns REAL, SEPARATE subprocesses (not threads, and
no mocked lock) because process-level mutual exclusion is exactly the
property under test -- a single-process test would assert the property into
existence rather than observe it.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
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
    exit code -- these must pass against the no-op AND against the real
    permit-gated wrapper."""

    def test_stdout_and_success_pass_through(self) -> None:
        result = subprocess.run(
            [sys.executable, str(WRAPPER), "--", "echo", "hi"],
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), "hi")

    def test_exit_code_passes_through(self) -> None:
        result = subprocess.run(
            [sys.executable, str(WRAPPER), "--", "sh", "-c", "exit 3"]
        )
        self.assertEqual(result.returncode, 3)


class ConcurrencyTest(unittest.TestCase):
    """The load-bearing test: with FLEET_BUILD_MAX=2, six concurrent
    wrapper invocations must never observe more than 2 running at once, and
    all six must complete.

    EXPECTED TO FAIL against the task-2 no-op wrapper, which takes no
    permit at all -- see module docstring.
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

            env = dict(os.environ)
            env["FLEET_BUILD_MAX"] = str(fleet_build_max)

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


if __name__ == "__main__":
    unittest.main()
