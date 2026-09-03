#!/usr/bin/env python3
"""Gate concurrent cargo (or any) build invocations with a fleet-build permit.

EN.ticket.fleet-build-permit-wrapper, task 2: this is a DELIBERATE NO-OP for
this task. It parses `-- <cmd...>`, exec's the wrapped command, and
propagates its exit code -- but takes NO permit at all yet. Task 3 turns
this into the real acquire/release-gated wrapper (count + free-memory
admission against `<BRAIN_ROOT>/.fleet-locks/builds/`, copying
`base-template/scripts/fleet_concurrency_check.py`'s TTL-expiry, stale-sweep
and fail-open semantics).

Per D68 / carryover `gate-scope-must-be-shown-capable-of-failing`, this
no-op exists so that `scripts/tests/test_fleet_build.py`'s concurrency test
can be observed RED against it first -- a concurrency test that has never
failed may simply be watching a machine that was not busy.

Usage:
    python3 scripts/fleet_build.py -- <command> [args...]

Exits with the wrapped command's own exit code. Prints nothing of its own to
stdout/stderr on the success path -- the wrapper must be transparent to the
wrapped command's output and exit code.
"""

from __future__ import annotations

import subprocess
import sys


def main(argv: list[str]) -> int:
    if "--" not in argv:
        print(
            "fleet_build.py: usage: fleet_build.py -- <command> [args...]",
            file=sys.stderr,
        )
        return 2

    sep_index = argv.index("--")
    command = argv[sep_index + 1 :]
    if not command:
        print(
            "fleet_build.py: no command given after '--'",
            file=sys.stderr,
        )
        return 2

    # NO-OP (task 2): no permit is acquired or released here. Task 3 wraps
    # this exec in permit acquire/release-in-finally.
    result = subprocess.run(command)
    return result.returncode


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
