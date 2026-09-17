#!/usr/bin/env python3
"""Authoring-time guard: a task's files[] must be able to satisfy the work assertion.

Two spec-authoring defects, both reproduced in core/okf-core on 2026-09-04, both producing a block
that bails with every substantive task passed:

  1. EMPTY files[]. renderWorkAssertion (.claude/workflows/sdlc-task.js:409, mirrored in
     sdlc-flow.js) runs `git diff --name-status HEAD~1 HEAD`, reads the task's files[], and aborts
     unless the two intersect via `grep -qFx`. Against an empty files[] nothing can ever intersect,
     so the task fails condition 2 on every attempt and every retry. It is enforced structurally at
     sdlc-task.js:2433 — the engine refuses to record the task done/passed without a positive
     workAssertionPassed — and it is HARDCODED, independent of harness.json: perTask: false only
     filters harness gating checks (gatingChecks(), :2051) and cannot reach it.
     Live case: OK.ticket.learning-artifact-missing-title-description task 2, bail_class 5, with
     all five of its validation_commands passing and the block's work complete.

  2. FLEET-ROOT-RELATIVE paths. Block records write paths as core/okf-core/src/foo.rs; the
     assertion compares against a git diff run INSIDE the repo, which emits src/foo.rs. grep -qFx
     is a whole-line match, so a verbatim copy is a guaranteed WORK_ASSERTION_ABORT.
     Live case: task 1 of the same block, one full attempt lost.

Neither is catchable at run time in a useful way: the engine can only report that the assertion
failed, by which point the work is done and the attempt is spent. This is the authoring-time half.

Exit 0 - clean.  Exit 1 - a task cannot pass its own work assertion.

Usage:
  python3 scripts/check_task_files.py [--planning <dir>] [--quiet]
"""

import argparse
import json
import os
import sys
from pathlib import Path

# Repo directory names that may legitimately prefix a path in a BLOCK record but must be stripped
# before the path reaches a task's files[]. Derived from brain.toml when available so this does not
# hardcode the fleet's shape; the literal fallback keeps the check useful in a standalone repo.
def repo_prefixes(planning_root: Path):
    root = planning_root.resolve()
    while root != root.parent and not (root / "brain.toml").is_file():
        root = root.parent
    if not (root / "brain.toml").is_file():
        return set()
    import re
    text = (root / "brain.toml").read_text(encoding="utf-8")
    return {p for p in re.findall(r'repo_path\s*=\s*"([^"]+)"', text) if p not in (".", "")}


def check_tasks_file(path: Path, prefixes: set):
    problems = []
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception as e:
        return [f"{path}: unreadable ({e})"]
    if not isinstance(data, list):
        return []

    for t in data:
        if not isinstance(t, dict):
            continue
        tid = t.get("task_id", "?")
        files = t.get("files")
        if files is None:
            continue

        if files == []:
            problems.append(
                f"{path}: task {tid} ({t.get('title','')!r}) declares files: [] — it can NEVER "
                f"pass the work assertion. Fold its validation into the last task that produces a "
                f"diff; a standalone Validate task is also redundant, since gates:true harness "
                f"checks already run after every task (D63)."
            )
            continue

        for f in files:
            if not isinstance(f, str):
                continue
            for p in prefixes:
                if f == p or f.startswith(p + "/"):
                    problems.append(
                        f"{path}: task {tid} declares {f!r}, which is FLEET-ROOT-relative. The "
                        f"work assertion compares against a git diff run inside the repo, which "
                        f"emits {f[len(p) + 1:]!r}. grep -qFx is a whole-line match, so this is a "
                        f"guaranteed WORK_ASSERTION_ABORT. Strip the {p!r} prefix."
                    )
                    break
    return problems


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--planning", default="planning")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    planning = Path(args.planning)
    if not planning.exists():
        print(f"task-files: no {planning}/ — nothing to check")
        return 0

    prefixes = repo_prefixes(planning)
    problems, n = [], 0
    for p in sorted(planning.rglob("tasks.json")):
        # Archived specs are historical record; fixing them changes nothing that will ever run.
        if f"{os.sep}archive{os.sep}" in str(p):
            continue
        n += 1
        problems.extend(check_tasks_file(p, prefixes))

    if not problems:
        if not args.quiet:
            print(f"task-files: OK — {n} tasks.json checked")
        return 0

    print(f"task-files: FAILED — {len(problems)} task(s) cannot pass their own work assertion")
    for pr in problems:
        print(f"  {pr}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
