#!/usr/bin/env python3
"""Authoring-time lint: resolve every repo-relative path a spec's tasks.json asserts.

WHY THIS EXISTS
---------------
A spec can name a script in `validation_commands` (or in a task's own `files[]`) that has never
existed anywhere in the repo, and nothing catches it until a run fails mid-flight — by which point
it reads as a missing dependency, not a bad spec. Measured 2026-08-24: three specs carried
`python3 scripts/check_harness_registration.py` for a script that has never existed in this fleet,
and one of those specs CLOSED with the phantom command still live in two of its tasks. Separately,
`BT.ticket.commander-report-has-no-no-change-shape` carried `git diff --quiet scripts/drain_log.py`
— a path that lives in the BRAIN repo, not this one — and failed regardless of the task's work.

Both are the same root cause: a path asserted in a plan that nobody resolved against the
filesystem. This lint resolves every candidate path in a tasks.json at authoring time, allowing a
path that an EARLIER task in the same spec creates (D45's ordering already guarantees that task ran
first), and reports anything else.

WHAT THIS DOES NOT DO
----------------------
It never executes anything, never reads planning/master-plan.md, and never modifies a spec — it
only detects. The sibling rule for `clears_when` predicates
(scripts/check_clears_when_predicates.py) is a separate, later task; this script's repo-root
derivation is deliberately identical to that rule's (Path(__file__)'s own location, never
`git rev-parse --show-toplevel`) so neither lint depends on which git root it happens to run from —
that ambiguity is exactly the defect the sibling rule exists to reject.

Usage:
  python3 scripts/check_spec_validation_commands.py [--planning <dir>] [--quiet]

Exit 0 — every candidate path resolves (or is created by an earlier same-spec task).
Exit 1 — at least one candidate path resolves nowhere.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]

# Extensions a candidate token pulled out of a shell command line must carry to count as a
# repo-owned path (as opposed to a flag value, a URL, a version string, or ordinary prose noise).
# files[] entries are NOT filtered by extension — they are already literal, single-purpose path
# strings, not tokens inside a free-form command line.
CANDIDATE_EXTENSIONS = {
    ".py", ".sh", ".js", ".md", ".json", ".yml", ".yaml", ".toml", ".ts", ".txt",
}

# A path-shaped run of characters inside a shell command string.
TOKEN_RE = re.compile(r"[A-Za-z0-9_./-]+")


def load_tasks(path: Path) -> list:
    """Return the bare task list from a tasks.json, tolerating the legacy {"tasks": [...]} wrapper.

    D45 settled the bare-array shape; this only READS the legacy wrapper for tolerance — it never
    writes one.
    """
    data = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(data, list):
        return data
    if isinstance(data, dict) and isinstance(data.get("tasks"), list):
        return data["tasks"]
    return []


def looks_repo_relative(token: str) -> bool:
    if not token or "/" not in token:
        return False
    return token[0] not in "-$/~"


def path_resolves(tok: str) -> bool:
    """A candidate path resolves if it exists relative to this repo's root, or — the fallback
    that matters for a validation_commands string that greps a DOC for a mention of a path that
    lives one repo up — relative to the BRAIN root. Some validation_commands strings check that a
    command file MENTIONS a path (e.g. `grep -q 'docs/sandbox/foo.md' some-command.md`) rather
    than opening that path itself, and the mentioned path is sometimes a brain-root doc, not a
    base-template one. Accepting either root avoids flagging that shape as a phantom path while
    still catching a genuinely nonexistent one under both."""
    if (REPO_ROOT / tok).exists():
        return True
    return (REPO_ROOT.parent / tok).exists()


def load_closed_spec_slugs(repo_root: Path) -> set:
    """Return the set of spec slugs (block ids) whose planning/state.json block is closed.

    A closed spec is historical record — CLAUDE.md/AGENTS.md standing rule: don't repair a
    phantom path in a spec that already shipped and closed; it stays as evidence of what the
    validation gap looked like before this lint existed. So it must not gate this check either,
    the same way an archived spec (moved under planning/archive/) already doesn't."""
    state_path = repo_root / "planning" / "state.json"
    try:
        data = json.loads(state_path.read_text(encoding="utf-8"))
    except Exception:
        return set()
    closed = set()
    for track in data.get("tracks") or []:
        for block in track.get("blocks") or []:
            if isinstance(block, dict) and block.get("status") == "closed":
                bid = block.get("id")
                if isinstance(bid, str):
                    closed.add(bid)
    return closed


def is_candidate_from_command(token: str) -> bool:
    if not looks_repo_relative(token):
        return False
    return Path(token).suffix in CANDIDATE_EXTENSIONS


def extract_command_candidates(command: str):
    """Yield path-shaped, repo-ownable tokens out of a free-form validation_commands string."""
    for raw in TOKEN_RE.findall(command):
        tok = raw.strip(",;")
        if is_candidate_from_command(tok):
            yield tok


def build_creates_map(tasks: list) -> dict:
    """Map each files[] path to the LOWEST task_id in this spec that declares it."""
    creates: dict[str, int] = {}
    for t in tasks:
        if not isinstance(t, dict):
            continue
        tid = t.get("task_id")
        if not isinstance(tid, int):
            continue
        for f in t.get("files") or []:
            if not isinstance(f, str):
                continue
            existing = creates.get(f)
            if existing is None or tid < existing:
                creates[f] = tid
    return creates


def check_spec(path: Path):
    """Return (violations, checked_count) for one tasks.json."""
    try:
        tasks = load_tasks(path)
    except Exception as e:
        return [f"{path}: unreadable ({e})"], 0

    spec_slug = path.parent.name
    creates = build_creates_map(tasks)
    violations = []
    checked = 0

    for t in tasks:
        if not isinstance(t, dict):
            continue
        tid = t.get("task_id", "?")

        # files[] entries: literal paths, no extension filter.
        for f in t.get("files") or []:
            if not isinstance(f, str) or not looks_repo_relative(f):
                continue
            checked += 1
            if path_resolves(f):
                continue
            creator = creates.get(f)
            # Legal when an earlier (or this same) task in the spec creates it — a task always
            # legitimately lists the very file it is about to produce.
            if isinstance(creator, int) and isinstance(tid, int) and creator <= tid:
                continue
            if isinstance(creator, int) and not isinstance(tid, int):
                continue
            violations.append(
                f"{spec_slug} task {tid}: files[] path {f!r} does not exist and is not created "
                f"by an earlier task in this spec"
            )

        # validation_commands: free-form shell strings, tokens extracted and extension-filtered.
        for cmd in t.get("validation_commands") or []:
            if not isinstance(cmd, str):
                continue
            for tok in extract_command_candidates(cmd):
                checked += 1
                if path_resolves(tok):
                    continue
                creator = creates.get(tok)
                if isinstance(creator, int) and isinstance(tid, int) and creator <= tid:
                    continue
                violations.append(
                    f"{spec_slug} task {tid}: validation_commands path {tok!r} does not exist and "
                    f"is not created by an earlier task in this spec (command: {cmd!r})"
                )

    return violations, checked


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--planning", default="planning")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    planning = Path(args.planning)
    if not planning.exists():
        print(f"spec-validation-command-paths: no {planning}/ — nothing to check")
        return 0

    all_violations = []
    specs_checked = 0
    paths_checked = 0
    closed_slugs = load_closed_spec_slugs(REPO_ROOT)

    for p in sorted(planning.rglob("tasks.json")):
        # Archived specs are historical record; a phantom path there changes nothing that will
        # ever run.
        if f"{os.sep}archive{os.sep}" in str(p):
            continue
        # A spec whose block already CLOSED is historical record too, by the same reasoning —
        # it will never run again, and repairing a stale path in it is explicitly out of scope
        # (the spec stays as evidence of what the gap looked like before this lint existed).
        if p.parent.name in closed_slugs:
            continue
        specs_checked += 1
        violations, checked = check_spec(p)
        paths_checked += checked
        all_violations.extend(violations)

    if not all_violations:
        if not args.quiet:
            print(
                f"spec-validation-command-paths: OK — {specs_checked} tasks.json checked, "
                f"{paths_checked} candidate path(s) resolved"
            )
        return 0

    print(
        f"spec-validation-command-paths: FAILED — {len(all_violations)} unresolved path(s) across "
        f"{specs_checked} tasks.json"
    )
    for v in all_violations:
        print(f"  {v}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
