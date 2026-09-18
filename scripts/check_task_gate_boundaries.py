#!/usr/bin/env python3
"""Fail a task whose gate cannot pass at the boundary it is asked to pass at
(BT.ticket.task-gate-boundaries-are-unenforced).

Both engines call `runTests(...)` INSIDE the per-task loop
(`.claude/workflows/sdlc-task.js:2062`), so every task boundary is a full gate, not just the
end of the spec: **every task must leave the gating suite passing.** That rule is stated in
prose in four commands (`/generate-tasks`, `/ticket`, `/chore`, `/breakdown`) but was never
enforced -- a hand-authored `tasks.json` skips all four at once, and did, four times in one
session (`BT.ticket.worktree-smoke-fixture` bailed at task 1 because its gate ran checks that
could not pass until later work landed).

This checker decides the one corollary that is mechanically decidable from `tasks.json`
alone -- it does NOT try to predict whether the whole harness suite passes at each boundary
(undecidable from the spec; a checker that guesses is noise the reader learns to ignore):

RULE 1 -- a task's own `validation_commands` must not reference a path that does not exist
yet at that boundary. For task N, build:

    UNSEEN(N) = (union of files[] over every task with a GREATER task_id)
                MINUS (union of files[] over tasks 1..N, i.e. this task and every earlier one)
                MINUS (anything that already exists on disk)

and flag any `validation_commands` string in task N that contains a path in UNSEEN(N).

BOTH subtractions are load-bearing -- found by running this rule against THIS block's own
`tasks.json` before committing it. The naive form ("references a path in a later task's
files[]") produces two false positives right here: task 1 legitimately names
`scripts/check_task_gate_boundaries.py`, which task 1 itself creates (own-files subtraction);
and task 1 legitimately reads `planning/harness.json`, which task 3 later modifies but which
already exists on disk (on-disk subtraction). The rule is "depends on a path that does not
exist yet", never "mentions a later task's file".

RULE 2 -- a task that edits `planning/harness.json` (i.e. registers a gate) while a LATER
task creates the script that gate would need. The actual command text being added to
`harness.json` is not visible from `tasks.json` alone, so this is deliberately the coarse,
conservative signal named in the block record: flag task N when `planning/harness.json` is in
its `files[]` and some task M with task_id > N creates a NEW `scripts/*.py` file (not already
on disk, not created by task N or earlier, and never previously tracked in git -- see below).
A false positive here trains readers to ignore the gate, which is worse than the miss -- so
this rule fires only on that narrow shape, never on a task merely reading or generically
mentioning harness.json.

A path that a later task's `files[]` names is not necessarily a CREATION -- it can just as
well be a deletion or edit of a script that already existed. `files[]` alone cannot tell the
two apart, and the on-disk check above only catches the case where the path is still present;
a task that DELETES a script (as opposed to creating one) leaves nothing on disk to find, so
without a further check the same on-disk subtraction that correctly clears "already exists"
would wrongly treat "already existed and was removed" as "does not exist yet". MEASURED
2026-08-28 against this repo's real corpus: `BT.5.B` task 2 registers `check_worked_example_
lane.py` into `harness.json`, and task 5 -- unrelated to that registration -- both deletes
`scripts/test_lane_directive_emission.py` and removes its own `harness.json` entry. The path
was never on disk when the checker ran (task 5 already landed and the file is gone), but
`git log --oneline --all -- scripts/test_lane_directive_emission.py` shows three prior
commits, so it plainly existed before this scan -- a deletion, not a future creation. Rule 2
therefore also excludes any path that has ever been git-tracked, via `_ever_tracked()`, before
concluding a later task "creates" it.

RULE 3 -- a task that tightens a gating threshold, floor, or detector when a LATER task in
the same tasks.json regenerates the artifact that detector reads. Neither RULE 1 nor RULE 2
can see this shape: RULE 1 is path-existence and here the artifact's path already exists on
disk at every boundary; RULE 2 keys on `planning/harness.json` registration and no gate is
being newly registered here -- only the artifact's CONTENT is stale at the boundary. This is
bella's real instance (BT.ticket.engines-cannot-express-a-red-green-task): its ticket bailed
at task 3 twice identically because task 3 sharpened a `gates:true` MIN_PNG_BYTES floor in a
detector script before task 4 re-captured the screenshot that same check reads -- no retry
could ever clear it, because the gate that had to pass at task 3's own boundary was checking
an artifact task 3 never touched.

Detected structurally, from the repo's OWN `planning/harness.json` (not the fixture-only
signal RULE 2 uses): for every check registered `gates:true` there, split its `command`
string on whitespace and take the first `scripts/*.py`-shaped token as the detector it
INVOKES and every other slash-containing token as a path it READS AS INPUT (e.g.
`python3 scripts/check_screenshot_floor.py assets/screenshot.png` invokes
`scripts/check_screenshot_floor.py` and reads `assets/screenshot.png`). Flag task N when its
`files[]` contains that detector path and some later task M (task_id > N) has that same
input path in its own `files[]` -- the detector changed before the artifact it grades did.

RULE 3 is the most false-positive-prone rule in this file, more so than RULE 2: it fires on
ANY edit to a detector script ahead of ANY later edit to an artifact it reads, with no way to
tell from `tasks.json` alone whether the earlier edit actually tightened a threshold (as
opposed to, say, a comment fix or a refactor that changes nothing observable) or whether the
later edit actually regenerates the artifact's content (as opposed to touching unrelated
bytes in the same file). It deliberately does NOT try to decide either of those -- exactly
the class of prediction this file's opening paragraph already refuses, because a checker that
guesses trains the reader to ignore it. Two things keep it narrow rather than noisy: it only
fires on paths that a REGISTERED `gates:true` check's own `command` names (never a path this
checker invents), and the negative control below -- a detector-script edit with no later task
touching the check's input path -- must NOT fire, so the rule cannot degenerate into "flag
every detector edit." The merged-task shape (one task edits both the detector and the
artifact together) also does not fire, since RULE 3 only ever compares a task to a STRICTLY
LATER one; that is the fix RULE 3 is telling the author to make, so it has to stay reachable.

A spec with no tasks.json, an empty array, or a task with no validation_commands is not a
failure -- say nothing about it. Every subprocess call (none here) would check its own return
code, and no check here is built on a shell pipeline.

Usage:
    check_task_gate_boundaries.py [--planning DIR] [--quiet]

    --planning DIR   scan one repo's planning/ (default: planning)
    --quiet          print only findings and the summary

Exit code 1 if any task-boundary violation is found across the scanned planning/*/tasks.json.
A repo with none is silent and exits 0.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess

SKIP_DIRS = {"node_modules", ".git", "archive", "target", ".fleet-locks", "sdlc", "trees", "blocks"}

HARNESS_PATH = "planning/harness.json"

_TRACKED_CACHE = {}
_HARNESS_CHECKS_CACHE = {}


def _ever_tracked(path, repo_root="."):
    """True if `path` has ever appeared in this repo's git history (any branch).

    Used by RULE 2 to tell "does not exist yet" from "existed and was deleted" -- a path
    with a git history is not a script a later task is about to create for the first time,
    even when it is absent from the working tree right now. Cached per path since the same
    later-task path can be checked from several earlier tasks' boundaries. A missing/failing
    git call (no repo, no git binary) is treated as "not tracked" -- it falls back to the
    on-disk-only signal rather than crashing the checker.
    """
    key = (repo_root, path)
    if key in _TRACKED_CACHE:
        return _TRACKED_CACHE[key]
    try:
        result = subprocess.run(
            ["git", "log", "--oneline", "--all", "-1", "--", path],
            cwd=repo_root, capture_output=True, text=True, timeout=10,
        )
        tracked = result.returncode == 0 and bool(result.stdout.strip())
    except Exception:  # noqa: BLE001 - no git available is not this checker's failure
        tracked = False
    _TRACKED_CACHE[key] = tracked
    return tracked


def _load_harness_checks(repo_root="."):
    """Return the RULE-3-relevant shape of every `gates:true` check registered in
    `{repo_root}/planning/harness.json`: a list of {"name": str, "script": str,
    "inputs": [str, ...]} dicts, one per check whose `command` names both a
    `scripts/*.py` detector and at least one other slash-containing argument.

    A check with no detector script, no other path argument, a malformed/missing
    `command`, or an absent/unparseable `harness.json` contributes nothing -- this is a
    RULE 3 input signal, not a harness-schema validator (that is a different check's
    job). Cached per repo_root since the same harness.json is read once per task in a
    spec's boundary scan.
    """
    key = os.path.abspath(repo_root)
    if key in _HARNESS_CHECKS_CACHE:
        return _HARNESS_CHECKS_CACHE[key]

    out = []
    path = os.path.join(repo_root, HARNESS_PATH)
    try:
        with open(path) as fh:
            data = json.load(fh)
    except Exception:  # noqa: BLE001 - a malformed harness.json is another check's job
        data = None

    if isinstance(data, dict):
        checks = (data.get("validation") or {}).get("checks") or []
        for c in checks:
            if not isinstance(c, dict) or not c.get("gates"):
                continue
            command = c.get("command")
            if not isinstance(command, str):
                continue
            script_path = None
            input_paths = []
            for tok in command.split():
                if tok.startswith("-") or "/" not in tok:
                    continue
                if script_path is None and tok.startswith("scripts/") and tok.endswith(".py"):
                    script_path = tok
                elif tok != script_path:
                    input_paths.append(tok)
            if script_path and input_paths:
                out.append({"name": c.get("name"), "script": script_path,
                            "inputs": input_paths})

    _HARNESS_CHECKS_CACHE[key] = out
    return out


def _load_tasks(path):
    """Return the tasks list, or None if the file is absent/unparseable/not-a-list."""
    try:
        with open(path) as fh:
            data = json.load(fh)
    except Exception:  # noqa: BLE001 - a malformed file is another check's job to report
        return None
    if not isinstance(data, list):
        return None
    return data


def _files_of(task):
    return {f for f in (task.get("files") or []) if isinstance(f, str)}


def _by_id(tasks):
    """Return {task_id: task} for every task with an integer task_id, skipping the rest."""
    out = {}
    for task in tasks:
        if not isinstance(task, dict):
            continue
        tid = task.get("task_id")
        if isinstance(tid, int) and not isinstance(tid, bool):
            out[tid] = task
    return out


def check_spec(tasks, repo_root="."):
    """Return a list of finding dicts for one spec's already-loaded tasks list.

    Each finding: {"rule": "R1"|"R2"|"R3", "task_id": int, "path": str, "later_task_id":
    int, "command": str|None}.
    """
    findings = []
    by_id = _by_id(tasks)
    if not by_id:
        return findings
    ids_sorted = sorted(by_id)
    harness_checks = _load_harness_checks(repo_root)

    for tid in ids_sorted:
        task = by_id[tid]

        earlier_and_own = set()
        for tid2 in ids_sorted:
            if tid2 <= tid:
                earlier_and_own |= _files_of(by_id[tid2])

        later_files = {}  # path -> earliest later task_id that creates it
        for tid2 in ids_sorted:
            if tid2 <= tid:
                continue
            for p in _files_of(by_id[tid2]):
                later_files.setdefault(p, tid2)

        unseen = {}
        for p, creator_tid in later_files.items():
            if p in earlier_and_own:
                continue
            if os.path.exists(os.path.join(repo_root, p)):
                continue
            unseen[p] = creator_tid

        # -- RULE 1: validation_commands referencing an UNSEEN path -------------------------
        for cmd in task.get("validation_commands") or []:
            if not isinstance(cmd, str):
                continue
            for p, creator_tid in unseen.items():
                if p in cmd:
                    findings.append({
                        "rule": "R1", "task_id": tid, "path": p,
                        "later_task_id": creator_tid, "command": cmd,
                    })

        # -- RULE 2: harness.json edited here, a new script arrives only later --------------
        if HARNESS_PATH in _files_of(task):
            seen_scripts = set()
            for p, creator_tid in later_files.items():
                if p in earlier_and_own or p in seen_scripts:
                    continue
                if not (p.startswith("scripts/") and p.endswith(".py")):
                    continue
                if os.path.exists(os.path.join(repo_root, p)):
                    continue
                if _ever_tracked(p, repo_root):
                    continue
                seen_scripts.add(p)
                findings.append({
                    "rule": "R2", "task_id": tid, "path": p,
                    "later_task_id": creator_tid, "command": None,
                })

        # -- RULE 3: a registered gates:true check's detector is tightened here while a --
        #            LATER task regenerates the artifact that same check reads as input --
        task_files = _files_of(task)
        for hc in harness_checks:
            if hc["script"] not in task_files:
                continue
            for tid2 in ids_sorted:
                if tid2 <= tid:
                    continue
                matched = sorted(_files_of(by_id[tid2]) & set(hc["inputs"]))
                if not matched:
                    continue
                for p in matched:
                    findings.append({
                        "rule": "R3", "task_id": tid, "path": p,
                        "later_task_id": tid2, "command": hc["name"],
                    })
                break  # earliest later task that regenerates any input is enough to flag

    return findings


def _format_finding(spec_file, finding):
    task_id = finding["task_id"]
    path = finding["path"]
    later = finding["later_task_id"]
    if finding["rule"] == "R1":
        return (f"FAIL {spec_file} task {task_id}: validation_commands references "
                f"{path!r}, which does not exist at this boundary -- created only by "
                f"task {later}. command: {finding['command']!r}")
    if finding["rule"] == "R3":
        return (f"FAIL {spec_file} task {task_id}: tightens the detector for gates:true "
                f"check {finding['command']!r} while the artifact it reads, {path!r}, is "
                f"not regenerated until later task {later} -- the gate cannot pass at "
                f"this boundary")
    return (f"FAIL {spec_file} task {task_id}: registers planning/harness.json (a gate) "
            f"while {path!r} is created only by later task {later} -- the gate cannot "
            f"pass at this boundary")


def find_tasks_json(planning_root):
    """Yield every planning/*/tasks.json path under `planning_root`, following symlinks
    (planning/ is a vaulted symlink in this fleet), skipping blocks/archive/vcs noise."""
    for dirpath, dirnames, filenames in os.walk(planning_root, followlinks=True):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        if "tasks.json" in filenames:
            yield os.path.join(dirpath, "tasks.json")


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--planning", default="planning")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args(argv)

    if not os.path.isdir(args.planning):
        print(f"no {args.planning}/ found (not a failure)")
        return 0

    spec_paths = sorted(find_tasks_json(args.planning))
    if not spec_paths:
        print("no planning/*/tasks.json found (not a failure)")
        return 0

    total_findings = 0
    for spec_path in spec_paths:
        tasks = _load_tasks(spec_path)
        if tasks is None:
            continue
        findings = check_spec(tasks, repo_root=".")
        if findings:
            for finding in findings:
                total_findings += 1
                print(_format_finding(spec_path, finding))
        elif not args.quiet:
            print(f"ok   {spec_path}")

    print(f"\n{len(spec_paths)} spec(s) checked, {total_findings} finding(s)")
    return 1 if total_findings else 0


if __name__ == "__main__":
    import sys
    sys.exit(main())
