"""Lint rule registry for check_tasks_json.py (BT.ticket.prepare-run-replaces-setup-agents, task 3).

REGISTRY is a list of {"id": str, "check": callable(tasks_json_path, harness_config) ->
list[finding], "fix_hint": str} entries. Every `check` callable is called with the same two
arguments regardless of what the underlying checker actually reads from disk — a tasks.json
path plus the repo's already-parsed planning/harness.json — so check_tasks_json.py's umbrella
loop stays uniform across rules that inspect wildly different artifacts (a spec's tasks.json, its
sibling block record, or harness.json's own `validation.checks[]`). Each `finding` is a
{"rule_id": str, "message": str} dict; `message` is byte-for-byte the same text the wrapped
script would have printed standalone, so wrapping introduces no behavior change (task 3
acceptance criterion 4).

Five of the six rules below wrap an existing standalone checker verbatim — same logic, same
inputs, same output text, just called as a function instead of a subprocess:

  - task-files                -> scripts/check_task_files.py (check_tasks_file)
  - spec-validation-commands  -> scripts/check_spec_validation_commands.py (check_spec)
  - task-gate-boundaries      -> scripts/check_task_gate_boundaries.py (check_spec), RULES 1-2
                                  only — RULE 3 (detector/artifact staleness) is out of scope for
                                  this registry and is filtered out of the wrapped result.
  - block-records             -> scripts/check_block_records.py (check), run against the block
                                  record co-located at planning/blocks/<spec-slug>.json, derived
                                  from the tasks.json path's parent directory name. A spec with no
                                  sibling block record (legacy layout, or a fixture with no
                                  planning/blocks/ entry) contributes no findings — this rule has
                                  nothing to check, which is not itself a violation.
  - observed-red              -> scripts/check_observed_red.py (evaluate_checks), run against
                                  harness_config['validation']['checks'] directly — this rule
                                  never reads the tasks.json at all.

The sixth, `no-diff-kind-validate`, is new (not wrapped from anywhere): a task whose `files` is
empty or absent and that does not declare `"kind": "validate"` is flagged — the FE.7.D shape
("a task changes a function's return value or output literal -> grep tests for the old literal;
stale assertions are fixed in the same task") reduced to what is staticly decidable from
tasks.json alone: a task that is *expected* to produce no diff must say so explicitly, or it
reads as unfinished work rather than a deliberate no-op.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path

# .claude/workflows/bin/lint_rules/__init__.py -> .claude/workflows/bin -> .claude/workflows ->
# .claude -> repo root. scripts/ lives at the repo root, sibling to .claude/.
_BIN_DIR = Path(__file__).resolve().parent.parent
_REPO_ROOT = _BIN_DIR.parent.parent.parent
_SCRIPTS_DIR = _REPO_ROOT / "scripts"

if str(_SCRIPTS_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPTS_DIR))

import check_task_files  # noqa: E402
import check_spec_validation_commands  # noqa: E402
import check_task_gate_boundaries  # noqa: E402
import check_block_records  # noqa: E402
import check_observed_red  # noqa: E402


def _finding(rule_id: str, message: str) -> dict:
    return {"rule_id": rule_id, "message": message}


def _check_task_files(tasks_json_path, harness_config):
    path = Path(tasks_json_path)
    prefixes = check_task_files.repo_prefixes(Path("planning"))
    problems = check_task_files.check_tasks_file(path, prefixes)
    return [_finding("task-files", p) for p in problems]


def _check_spec_validation_commands(tasks_json_path, harness_config):
    path = Path(tasks_json_path)
    violations, _checked = check_spec_validation_commands.check_spec(path)
    return [_finding("spec-validation-commands", v) for v in violations]


def _check_task_gate_boundaries(tasks_json_path, harness_config):
    path = Path(tasks_json_path)
    tasks = check_task_gate_boundaries._load_tasks(str(path))
    if tasks is None:
        return []
    findings = check_task_gate_boundaries.check_spec(tasks, repo_root=".")
    out = []
    for f in findings:
        # This registry entry wraps RULES 1-2 only (per task 3's scope) — RULE 3 (detector/
        # artifact staleness) is deliberately excluded here.
        if f.get("rule") not in ("R1", "R2"):
            continue
        out.append(_finding(
            "task-gate-boundaries",
            check_task_gate_boundaries._format_finding(str(path), f),
        ))
    return out


def _check_block_records(tasks_json_path, harness_config):
    path = Path(tasks_json_path)
    spec_slug = path.parent.name
    block_path = Path("planning") / "blocks" / f"{spec_slug}.json"
    if not block_path.is_file():
        return []
    planning_root = "planning"
    planning_is_symlinked = os.path.islink(planning_root)
    problems, _warnings = check_block_records.check(
        str(block_path), planning_root=planning_root,
        planning_is_symlinked=planning_is_symlinked,
    )
    return [_finding("block-records", f"{block_path}: {p}") for p in problems]


def _check_observed_red(tasks_json_path, harness_config):
    checks = ((harness_config or {}).get("validation") or {}).get("checks") or []
    details = check_observed_red.evaluate_checks(checks)
    return [_finding("observed-red", d) for d in details]


def _check_no_diff_kind_validate(tasks_json_path, harness_config):
    path = Path(tasks_json_path)
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except Exception as e:  # noqa: BLE001 - report, never raise
        return [_finding("no-diff-kind-validate", f"{path}: unreadable ({e})")]
    if not isinstance(data, list):
        return []

    findings = []
    for t in data:
        if not isinstance(t, dict):
            continue
        tid = t.get("task_id", "?")
        files = t.get("files")
        empty = files is None or files == []
        if empty and t.get("kind") != "validate":
            findings.append(_finding(
                "no-diff-kind-validate",
                f"{path}: task {tid} ({t.get('title', '')!r}) declares no files (an empty diff) "
                f"and no `kind: \"validate\"` field — a task that legitimately produces no diff "
                f"must say so explicitly via kind: validate (FE.7.D: fix stale assertions in the "
                f"same task that changes the literal, rather than leaving an unmarked no-diff "
                f"task that reads as unfinished work).",
            ))
    return findings


REGISTRY = [
    {
        "id": "task-files",
        "check": _check_task_files,
        "fix_hint": (
            "Fold the empty-files[] task's validation into the last task that produces a diff "
            "(a standalone Validate task is redundant — gates:true harness checks already run "
            "after every task, D63), or strip the fleet-root-relative path prefix. See "
            "scripts/check_task_files.py."
        ),
    },
    {
        "id": "spec-validation-commands",
        "check": _check_spec_validation_commands,
        "fix_hint": (
            "Fix the phantom path, or move the file into an earlier (or this) task's files[] if "
            "this spec creates it. See scripts/check_spec_validation_commands.py."
        ),
    },
    {
        "id": "task-gate-boundaries",
        "check": _check_task_gate_boundaries,
        "fix_hint": (
            "Reorder so a task's own validation_commands only reference paths that already "
            "exist at its boundary, or move the planning/harness.json registration to a later "
            "task. See scripts/check_task_gate_boundaries.py (rules 1-2 only)."
        ),
    },
    {
        "id": "block-records",
        "check": _check_block_records,
        "fix_hint": (
            "Fix the block record's required/enum fields at "
            "planning/blocks/<spec-slug>.json. See scripts/check_block_records.py."
        ),
    },
    {
        "id": "observed-red",
        "check": _check_observed_red,
        "fix_hint": (
            "Add a valid observed_red {date, evidence} to every gates:true check in "
            "planning/harness.json. See scripts/check_observed_red.py."
        ),
    },
    {
        "id": "no-diff-kind-validate",
        "check": _check_no_diff_kind_validate,
        "fix_hint": (
            'Add "kind": "validate" to a task that legitimately produces no diff, or give it '
            "files[] that will actually change."
        ),
    },
]
