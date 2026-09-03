#!/usr/bin/env python3
"""Gate concurrent cargo (or any) build invocations with a fleet-build permit.

EN.ticket.fleet-build-permit-wrapper, task 3: the real permit-gated wrapper.
It resolves `<BRAIN_ROOT>` by the same `brain.toml` walk-up the rest of the
fleet uses, takes a short-lived permit from a NEW
`<BRAIN_ROOT>/.fleet-locks/builds/` namespace (sibling to `leases/`,
`lane-agents/`, `commander-heartbeats/`, `queue/`), exec's the wrapped
command, and releases the permit in a `finally` so a killed build cannot
strand it.

Admission requires BOTH:
  - a free permit slot, bounded by `FLEET_BUILD_MAX` (default 2), and
  - free memory above `FLEET_BUILD_MIN_FREE_MB` (default 2048, from the
    peak-RSS measurement in scripts/tests/fixtures/link-rss-measurement.txt)
re-checked at EACH dequeue attempt, never once at enqueue -- a permit
granted while a queue drains would otherwise be stale by the time it is
used.

Semantics are COPIED from `base-template/scripts/fleet_concurrency_check.py`
deliberately, not invented fresh: TTL-based expiry of stranded entries,
stale-entry sweeping on every acquire attempt, and above all its fail-open
philosophy -- if the lock store cannot be resolved, created, or written to,
the wrapper runs the command anyway and says so on stderr
(`degraded: true, allowed: true`). A build gate that can hard-fail a build
is worse than no gate.

ONE DELIBERATE DEPARTURE from that prior art: `fleet_concurrency_check.py`'s
CLI process is short-lived (it registers and exits immediately), so its own
pid is never a valid liveness signal for a later checker. This wrapper's
process, by contrast, stays alive for the FULL duration of the wrapped
command -- so its own pid IS a meaningful liveness signal here, and permit
entries are swept the instant their holder's pid is no longer running,
rather than only after the TTL. That is what makes a SIGKILLed build's
permit reclaimable immediately instead of waiting out the TTL, as a second
line of defense alongside the normal `finally`-based release.

TTL here is MINUTES, not `fleet_concurrency_check.py`'s `DEFAULT_TTL_SECONDS
= 5400` -- that value is tuned to a ~90-minute lane segment; a build permit
held that long by a dead process would stall the fleet's build queue for
the better part of two hours. `FLEET_BUILD_TTL_SECONDS` defaults to 300s
(5 minutes), comfortably longer than any real build but short enough that a
stranded permit (one whose liveness check somehow missed, e.g. a
same-pid-reused edge case) self-heals fast.

Usage:
    python3 scripts/fleet_build.py -- <command> [args...]

Exits with the wrapped command's own exit code. Prints nothing of its own
to stdout/stderr on the success path -- the wrapper must be transparent to
the wrapped command's output and exit code. Only the acquire/degrade path
(and, transiently, a wait-for-admission notice) writes to stderr.

Environment variables:
    FLEET_BUILD_MAX              Max concurrent permits (default: 2)
    FLEET_BUILD_MIN_FREE_MB      Min free memory (MB) to admit (default: 2048)
    FLEET_BUILD_TTL_SECONDS      Stranded-permit TTL, seconds (default: 300)
    FLEET_BUILD_LOCK_DIR         Force the builds/ lock dir directly (tests)
    FLEET_LOCK_DIR               Shared `.fleet-locks/` root override (same
                                  var `fleet_concurrency_check.py` honours);
                                  this wrapper uses `<that>/builds`
    FLEET_BUILD_FREE_MB_OVERRIDE Force a single constant free-MB reading
                                  (tests; skips vm_stat entirely)
    FLEET_BUILD_FREE_MB_SEQUENCE_FILE
                                  Path to a file of newline-separated free-MB
                                  readings; each admission attempt pops the
                                  next line (the last line repeats once
                                  exhausted). Proves the memory check runs
                                  per-dequeue-attempt, not once at enqueue.
"""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import sys
import time
import uuid
from pathlib import Path
from typing import Optional

DEFAULT_FLEET_BUILD_MAX = 2
DEFAULT_MIN_FREE_MB = 2048  # see scripts/tests/fixtures/link-rss-measurement.txt
DEFAULT_TTL_SECONDS = 300  # 5 minutes -- see module docstring for why not 5400s
LOCK_SUBDIR = Path(".fleet-locks") / "builds"
POLL_INTERVAL_SECONDS = 0.05

_VM_STAT_PAGE_SIZE_RE = re.compile(r"page size of (\d+) bytes")
_VM_STAT_FIELD_RE = re.compile(r"^(Pages [a-z ]+):\s+(\d+)\.?\s*$", re.MULTILINE)


def find_brain_root(start: Optional[Path] = None) -> Optional[Path]:
    """Walk upward from `start` (default: cwd) looking for a brain.toml."""
    current = (start or Path.cwd()).resolve()
    for candidate in [current, *current.parents]:
        if (candidate / "brain.toml").exists():
            return candidate
    return None


def resolve_lock_dir() -> Optional[Path]:
    """Resolve the builds/ permit directory, or None if it cannot be used.

    Precedence: FLEET_BUILD_LOCK_DIR (points straight at the builds/ dir,
    used by tests), FLEET_LOCK_DIR (the shared `.fleet-locks/` root that
    `fleet_concurrency_check.py` also honours -- this wrapper appends
    `builds/`), then a brain.toml discovered by walking up from cwd. Returns
    None (never raises) when nothing resolves or the directory cannot be
    created/written -- callers must treat None as "degrade to advisory."
    """
    candidate: Optional[Path]
    if os.environ.get("FLEET_BUILD_LOCK_DIR"):
        candidate = Path(os.environ["FLEET_BUILD_LOCK_DIR"])
    elif os.environ.get("FLEET_LOCK_DIR"):
        candidate = Path(os.environ["FLEET_LOCK_DIR"]) / "builds"
    else:
        brain_root = find_brain_root()
        if brain_root is None:
            return None
        candidate = brain_root / LOCK_SUBDIR

    try:
        candidate.mkdir(parents=True, exist_ok=True)
        probe = candidate / f".probe-{os.getpid()}"
        probe.write_text("")
        probe.unlink()
    except OSError:
        return None

    return candidate


def _pid_running(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except OSError:
        return False
    return True


def _sweep_stale(lock_dir: Path, ttl_seconds: int) -> None:
    """Remove stale permit entries in place.

    An entry is stale when its recorded holder pid is no longer running
    (immediate reclaim -- e.g. a SIGKILLed build whose wrapper never got to
    run its `finally`) OR when it has simply outlived the TTL (a
    belt-and-braces catch for anything the liveness check missed).
    """
    now = time.time()
    for entry_path in sorted(lock_dir.glob("*.json")):
        try:
            data = json.loads(entry_path.read_text())
        except (OSError, json.JSONDecodeError):
            entry_path.unlink(missing_ok=True)
            continue

        pid = data.get("pid")
        started_at = data.get("started_at", 0)
        age = now - started_at
        alive = isinstance(pid, int) and _pid_running(pid)
        if (not alive) or age > ttl_seconds:
            entry_path.unlink(missing_ok=True)


def _pop_sequence_value(path: Path) -> int:
    """Pop the next free-MB reading from an injected test sequence file.

    Once only one line remains, it is left in place and returned on every
    subsequent call -- callers that read past the scripted sequence keep
    seeing the last (typically "now it's fine") value rather than erroring.
    """
    lines = [ln for ln in path.read_text().splitlines() if ln.strip() != ""]
    if not lines:
        return 0
    value = int(lines[0].strip())
    if len(lines) > 1:
        path.write_text("\n".join(lines[1:]) + "\n")
    return value


def _vm_stat_free_mb() -> float:
    """Estimate available memory (MB) from `vm_stat`.

    Sums "Pages free" + "Pages inactive" -- both reclaimable without swap
    activity -- rather than "Pages free" alone, which reads misleadingly
    low on macOS under the compressor (see
    scripts/tests/fixtures/link-rss-measurement.txt for the measured
    rationale). If `vm_stat` cannot be read at all, return +inf so an
    unreadable metric never itself blocks a build -- consistent with this
    wrapper's overall fail-open posture.
    """
    try:
        out = subprocess.run(
            ["vm_stat"], capture_output=True, text=True, timeout=5, check=False
        ).stdout
    except (OSError, subprocess.TimeoutExpired):
        return float("inf")

    size_match = _VM_STAT_PAGE_SIZE_RE.search(out)
    page_size = int(size_match.group(1)) if size_match else 4096

    fields = dict(_VM_STAT_FIELD_RE.findall(out))
    try:
        free_pages = int(fields.get("Pages free", "0"))
        inactive_pages = int(fields.get("Pages inactive", "0"))
    except ValueError:
        return float("inf")

    return (free_pages + inactive_pages) * page_size / (1024 * 1024)


def read_free_mb() -> float:
    """The free-memory reading the admission check uses.

    Test seams take priority over the real `vm_stat` read: a single
    constant override, or a sequence file popped once per call (used to
    prove the memory check runs at each dequeue attempt, not once at
    enqueue).
    """
    override = os.environ.get("FLEET_BUILD_FREE_MB_OVERRIDE")
    if override is not None:
        return float(override)

    seq_file = os.environ.get("FLEET_BUILD_FREE_MB_SEQUENCE_FILE")
    if seq_file is not None:
        return float(_pop_sequence_value(Path(seq_file)))

    return _vm_stat_free_mb()


def acquire_permit(
    lock_dir: Path, max_permits: int, min_free_mb: float, ttl_seconds: int
) -> Path:
    """Block until a permit slot is free, then claim and return its path.

    The check-and-claim critical section is serialized across processes
    with an flock on a dedicated `.admission.lock` file in `lock_dir`, so
    concurrent acquirers cannot both observe "one slot free" and both claim
    it (a plain count-then-write is racy under real concurrent processes,
    which is exactly what this wrapper is gating).
    """
    my_id = f"{os.getpid()}-{uuid.uuid4().hex[:8]}"
    permit_path = lock_dir / f"{my_id}.json"
    admission_lock_path = lock_dir / ".admission.lock"

    while True:
        with open(admission_lock_path, "a+") as lockf:
            fcntl.flock(lockf, fcntl.LOCK_EX)
            try:
                _sweep_stale(lock_dir, ttl_seconds)
                current = list(lock_dir.glob("*.json"))
                free_mb = read_free_mb()
                if len(current) < max_permits and free_mb >= min_free_mb:
                    data = {"pid": os.getpid(), "started_at": time.time()}
                    permit_path.write_text(json.dumps(data))
                    return permit_path
            finally:
                fcntl.flock(lockf, fcntl.LOCK_UN)
        time.sleep(POLL_INTERVAL_SECONDS)


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

    max_permits = int(os.environ.get("FLEET_BUILD_MAX", str(DEFAULT_FLEET_BUILD_MAX)))
    min_free_mb = float(
        os.environ.get("FLEET_BUILD_MIN_FREE_MB", str(DEFAULT_MIN_FREE_MB))
    )
    ttl_seconds = int(
        os.environ.get("FLEET_BUILD_TTL_SECONDS", str(DEFAULT_TTL_SECONDS))
    )

    permit_path: Optional[Path] = None
    lock_dir = resolve_lock_dir()
    if lock_dir is None:
        print(
            "fleet_build.py: lock store unavailable (no brain.toml found or "
            "directory unwritable) -- degraded: true, allowed: true, "
            "running without a permit",
            file=sys.stderr,
        )
    else:
        try:
            permit_path = acquire_permit(lock_dir, max_permits, min_free_mb, ttl_seconds)
        except OSError as exc:
            print(
                f"fleet_build.py: lock store unwritable ({exc}) -- "
                "degraded: true, allowed: true, running without a permit",
                file=sys.stderr,
            )
            permit_path = None

    try:
        result = subprocess.run(command, check=False)
        return result.returncode
    finally:
        if permit_path is not None:
            permit_path.unlink(missing_ok=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
