#!/usr/bin/env python3
"""Prove an engine-written lane-log.jsonl is readable by the REAL reader.

`EN.ticket.lane-log-entry-schema` Task 4's acceptance criterion is that a line the
engine appends parses under `scripts/roadmap_status_discovery.py`'s `read_lane_log`
and yields a non-empty `repo`, `lane`, `block` and `status` — "demonstrated against
the real script, not a reimplementation of it". This module does not reimplement
`read_lane_log` / `repos_from_lane_log`; it imports them directly from the brain
root's `scripts/roadmap_status_discovery.py` (located by walking up from this file
for `brain.toml`, the same rule `engine-core::brain_root` uses) and calls them.

Usage:
    verify_lane_log_readable.py <roadmap_dir>

Prints one JSON object to stdout: `{"entries": [...], "repos": [...]}`, where
`entries` is exactly what `read_lane_log` returned (list of dicts) and `repos` is
exactly what `repos_from_lane_log` returned (list of strings) — both unmodified, so
a caller asserting on this output is asserting on the real reader's real behaviour.
Exits non-zero (with a message on stderr) if `brain.toml` or the reader script
cannot be found, or if the target directory does not exist.
"""

from __future__ import annotations

import importlib.util
import json
import sys
from pathlib import Path


def find_brain_root(start: Path) -> Path:
    """Walk up from `start` looking for `brain.toml` — mirrors
    `engine-core::brain_root::resolve_brain_root_from`'s rule, so this script finds
    the same root the engine itself would."""
    current = start.resolve()
    for candidate in [current, *current.parents]:
        if (candidate / "brain.toml").is_file():
            return candidate
    raise SystemExit(f"no brain.toml found walking up from '{start}'")


def load_reader_module(brain_root: Path):
    """Import `scripts/roadmap_status_discovery.py` from the brain root by path —
    not by reimplementing it — so `read_lane_log` / `repos_from_lane_log` here are
    the exact same functions `/roadmap-status` and `/consolidate-run` call."""
    reader_path = brain_root / "scripts" / "roadmap_status_discovery.py"
    if not reader_path.is_file():
        raise SystemExit(f"reader script not found at '{reader_path}'")
    spec = importlib.util.spec_from_file_location("roadmap_status_discovery", reader_path)
    if spec is None or spec.loader is None:
        raise SystemExit(f"could not load module spec for '{reader_path}'")
    module = importlib.util.module_from_spec(spec)
    # Register in sys.modules BEFORE exec: the reader script defines `@dataclass`
    # classes, and `dataclasses` resolves a class's module via `sys.modules` while
    # processing the decorator — an unregistered module makes that lookup return
    # `None` and crash, even though nothing else about the import is wrong.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: verify_lane_log_readable.py <roadmap_dir>", file=sys.stderr)
        return 2
    roadmap_dir = Path(sys.argv[1])
    if not roadmap_dir.is_dir():
        print(f"roadmap dir does not exist: {roadmap_dir}", file=sys.stderr)
        return 2

    brain_root = find_brain_root(Path(__file__))
    reader = load_reader_module(brain_root)

    entries = reader.read_lane_log(roadmap_dir)
    repos = reader.repos_from_lane_log(entries)

    print(json.dumps({"entries": entries, "repos": repos}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
