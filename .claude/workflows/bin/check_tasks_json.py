#!/usr/bin/env python3
"""check_tasks_json.py — umbrella CLI over the lint rule registry (BT.ticket.prepare-run-replaces
-setup-agents, task 3).

Reads a tasks.json path plus the repo's planning/harness.json, resolves which
`.claude/workflows/bin/lint_rules` registry rules are ENABLED via harness.json's `lintRules`
object (task 1's schema addition — a rule id absent from `lintRules` defaults to enabled), runs
every enabled rule against the same (tasks_json_path, harness_config) pair, and prints a
pass/fail verdict plus every finding.

This is the same umbrella `prepare_run.py` (task 4) folds into its own `lint` output key, and the
same command `/generate-tasks`, `/ticket`, `/chore` and `/breakdown` run as their last step
before committing a new tasks.json (task 8).

Usage:
    python3 .claude/workflows/bin/check_tasks_json.py <tasks_json_path> [--harness PATH] [--quiet]

    --harness PATH   path to the harness.json to resolve `lintRules` from and to hand to every
                     rule as harness_config (default: planning/harness.json, repo-relative).
    --quiet          print only findings and the summary; suppress the OK line.

Exit 0 — every enabled rule ran with zero findings.
Exit 1 — at least one enabled rule reported a finding.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

_BIN_DIR = Path(__file__).resolve().parent
if str(_BIN_DIR) not in sys.path:
    sys.path.insert(0, str(_BIN_DIR))

import lint_rules  # noqa: E402


def load_harness_config(harness_path: Path) -> dict:
    """Return the parsed harness.json, or {} if it is absent/unreadable — a missing or malformed
    harness.json is not this umbrella's failure to report; every rule already tolerates an empty
    harness_config (each wrapped checker treats a missing config as "nothing extra to check")."""
    try:
        data = json.loads(harness_path.read_text(encoding="utf-8"))
    except Exception:  # noqa: BLE001 - report nothing extra, let the rules run with {}
        return {}
    return data if isinstance(data, dict) else {}


def resolve_enabled(registry, lint_rules_config: dict):
    """Return the registry entries that are enabled per `lintRules`. A rule id absent from
    `lintRules` entirely defaults to enabled; an explicit `{"enabled": false}` disables it with
    no code change — the config-over-code contract this registry exists to prove (AGENTS.md
    standing rule 12)."""
    enabled = []
    for rule in registry:
        rule_cfg = lint_rules_config.get(rule["id"]) or {}
        if rule_cfg.get("enabled", True):
            enabled.append(rule)
    return enabled


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                  formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("tasks_json", help="path to the spec's tasks.json")
    ap.add_argument("--harness", default="planning/harness.json",
                     help="path to harness.json (default: planning/harness.json)")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    tasks_json_path = Path(args.tasks_json)
    harness_config = load_harness_config(Path(args.harness))
    lint_rules_config = harness_config.get("lintRules") or {}

    enabled_rules = resolve_enabled(lint_rules.REGISTRY, lint_rules_config)

    all_findings = []
    for rule in enabled_rules:
        findings = rule["check"](tasks_json_path, harness_config)
        for f in findings:
            all_findings.append((rule["id"], rule["fix_hint"], f))

    if not all_findings:
        if not args.quiet:
            print(
                f"check-tasks-json: OK — {tasks_json_path} — {len(enabled_rules)} of "
                f"{len(lint_rules.REGISTRY)} rule(s) enabled, 0 finding(s)"
            )
        return 0

    print(
        f"check-tasks-json: FAILED — {len(all_findings)} finding(s) across "
        f"{len(enabled_rules)} of {len(lint_rules.REGISTRY)} enabled rule(s)"
    )
    for rule_id, fix_hint, finding in all_findings:
        message = finding.get("message", str(finding)) if isinstance(finding, dict) else str(finding)
        print(f"  [{rule_id}] {message}")
        print(f"      fix: {fix_hint}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
