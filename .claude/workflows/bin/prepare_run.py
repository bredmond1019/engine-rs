#!/usr/bin/env python3
"""prepare_run.py — one deterministic call that replaces the SDLC engines' 8 mechanical setup
agents (resolve-repo-root, detect-vault, verify-setup-binding, render-agent-flag,
render-scope-flag, harness-config, enumerate, state-load).

BT.ticket.prepare-run-replaces-setup-agents, task 2: this script computes ONLY the setup facts
those agents mechanically derive today — repo root, vault detection, the rendered `--agent`/
`--scope` mev flags, the project's harness.json parsed directly (never model-copied), and the
target spec's tasks.json enumeration (task_id + dependsOn per task). It makes NO network or LLM
call — every fact below is read straight off the filesystem or a `git`/`python3` subprocess, the
same mechanism the setup agents were instructed to run verbatim and merely transcribe.

Task 4 adds: the check_tasks_json.py lint verdict (folded in under the `lint` key), runnability
probes for every harness.json validation.checks[] entry carrying a `probeCommand`, and a refusal
contract — a missing `requires.bins`/`requires.env`/`requires.services` or a failing
`probeCommand` makes this script exit non-zero and print ONLY `{"refused": true, "reason": ...}`,
before any other setup fact is computed or printed. This is what lets an engine skip spinning up
an implement agent against an unrunnable spec (block AC5/AC6).

Usage:
  python3 .claude/workflows/bin/prepare_run.py --spec-slug <slug> [--repo-root <path>]
                                                [--simulate-missing-env VAR ...]

  --simulate-missing-env VAR   Test-only hook: treat VAR as a required environment variable (as
                                if some check declared `requires.env: [VAR]`) and refuse if it is
                                unset, WITHOUT needing a real harness.json check to declare it.
                                Repeatable. Checked before any real harness.json requires/probe.

Prints one JSON object to stdout. On success:
  {
    "repo_root": "<abs path>",
    "is_vaulted": true|false,
    "vault_root": "<abs path — realpath of planning/, same whether vaulted or not>",
    "agent_flag": " --agent <slug>" | "",
    "scope_flag": " --scope <slug>" | "",
    "harness_config": <planning/harness.json parsed, or null if absent/invalid>,
    "tasks_enumeration": [{"task_id": 1, "dependsOn": [...]}, ...],
    "lint": {"passed": bool, "findings": [...], "enabled_rules": int, "total_rules": int},
    "probes": [{"check": "<name>", "command": "<probeCommand>", "passed": true}, ...],
    "refused": false
  }

On refusal (nonzero exit), ONLY:
  {"refused": true, "reason": "<what failed, naming the check/command/variable and any stderr>"}
"""

import argparse
import json
import os
import shutil
import socket
import subprocess
import sys
from pathlib import Path

_BIN_DIR = os.path.dirname(os.path.abspath(__file__))
if _BIN_DIR not in sys.path:
    sys.path.insert(0, _BIN_DIR)

import check_tasks_json  # noqa: E402 — task 3's umbrella (load_harness_config, resolve_enabled)
import lint_rules  # noqa: E402 — task 3's registry


def _run_git_show_toplevel(cwd):
    """Mirrors resolveRepoRoot()'s `git rev-parse --show-toplevel`, run from cwd."""
    result = subprocess.run(
        ['git', 'rev-parse', '--show-toplevel'],
        cwd=cwd, capture_output=True, text=True,
    )
    if result.returncode != 0:
        return None
    return result.stdout.strip()


def resolve_repo_root(explicit_repo_root):
    """resolve-repo-root: REPO_ROOT via `git rev-parse --show-toplevel` from the invoking
    directory — no cd, no re-derivation, mirroring resolveRepoRoot() in sdlc-task.js/sdlc-flow.js."""
    if explicit_repo_root:
        return os.path.abspath(explicit_repo_root)
    root = _run_git_show_toplevel(os.getcwd())
    return root


def detect_vault(repo_root):
    """detect-vault: planning/ is a symlink (brain-vaulted) or a plain directory. Mirrors
    detectPlanningVault() — vaulted iff planning/ is a symlink; vault_root is planning/'s
    resolved real path either way (realpath of a plain directory is itself)."""
    planning_path = os.path.join(repo_root, 'planning')
    vaulted = os.path.islink(planning_path)
    vault_root = os.path.realpath(planning_path)
    return vaulted, vault_root


def _find_brain_root(start):
    """Walk up from `start` looking for brain.toml — same walk-up as renderAgentFlag() /
    renderScopeFlag()'s embedded python probes."""
    d = os.path.abspath(start)
    while True:
        if os.path.exists(os.path.join(d, 'brain.toml')):
            return d
        parent = os.path.dirname(d)
        if parent == d:
            return None
        d = parent


def _repo_blocks(text):
    blocks, cur = [], None
    for line in text.splitlines():
        if line.strip() == '[[repos]]':
            cur = []
            blocks.append(cur)
        elif cur is not None:
            cur.append(line)
    return ['\n'.join(b) for b in blocks]


def _block_value(block_text, key):
    for line in block_text.splitlines():
        s = line.strip()
        eq = s.find('=')
        if eq == -1 or s[:eq].strip() != key:
            continue
        v = s[eq + 1:].strip()
        if len(v) >= 2 and v[0] == '"' and v[-1] == '"':
            return v[1:-1]
        return None
    return None


def _best_slug(brain_root, here):
    with open(os.path.join(brain_root, 'brain.toml')) as f:
        text = f.read()
    here = os.path.abspath(here)
    best, best_depth = None, -1
    for block in _repo_blocks(text):
        slug = _block_value(block, 'slug')
        repo_path = _block_value(block, 'repo_path')
        if not slug or not repo_path:
            continue
        repo_abs = os.path.abspath(os.path.join(brain_root, repo_path))
        if here != repo_abs and not here.startswith(repo_abs + os.sep):
            continue
        depth = len(repo_abs.split(os.sep))
        if depth > best_depth:
            best_depth, best = depth, slug
    return best


def render_agent_flag(cwd):
    """render-agent-flag: FLEET_LANE_AGENT env var, else the lease file for this lane's slug
    under FLEET_LOCK_DIR (default <brain_root>/.fleet-locks/leases/lease-<slug>.json). Mirrors
    renderAgentFlag() field-for-field, including its "never fails destructively" contract —
    any missing/unreadable input degrades to no flag, never an exception."""
    env_agent = os.environ.get('FLEET_LANE_AGENT', '').strip()
    if env_agent:
        return f' --agent {env_agent}'

    try:
        brain_root = _find_brain_root(cwd)
        if not brain_root:
            return ''
        slug = _best_slug(brain_root, cwd)
        if not slug:
            return ''
        lock_dir = os.environ.get('FLEET_LOCK_DIR', '').strip() or os.path.join(brain_root, '.fleet-locks')
        lease_path = os.path.join(lock_dir, 'leases', f'lease-{slug}.json')
        if not os.path.exists(lease_path):
            return ''
        with open(lease_path) as f:
            lease = json.load(f)
        resolved = str(lease.get('agent') or '').strip()
    except Exception:
        resolved = ''
    return f' --agent {resolved}' if resolved else ''


def render_scope_flag(cwd):
    """render-scope-flag: FLEET_LANE_REPO env var, else the same brain.toml walk-up used by
    render_agent_flag(), yielding that entry's slug. Mirrors renderScopeFlag() field-for-field."""
    env_repo = os.environ.get('FLEET_LANE_REPO', '').strip()
    if env_repo:
        return f' --scope {env_repo}'

    try:
        brain_root = _find_brain_root(cwd)
        if not brain_root:
            return ''
        slug = _best_slug(brain_root, cwd)
    except Exception:
        slug = None
    return f' --scope {slug}' if slug else ''


def load_harness_config(repo_root):
    """harness-config: read planning/harness.json BY CODE and parse it directly — never a
    model-copied summary (the block's `why`: harness-config ran 432 times on Sonnet because
    Haiku cannot fill its nested StructuredOutput). Returns None if absent or invalid JSON,
    mirroring loadHarnessConfig()'s present=false degrade-to-spec-fallback contract (D5 /
    standing rule 1) rather than raising."""
    harness_path = os.path.join(repo_root, 'planning', 'harness.json')
    if not os.path.exists(harness_path):
        return None
    try:
        with open(harness_path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def enumerate_tasks(repo_root, spec_slug):
    """enumerate: read planning/<spec-slug>/tasks.json (a bare array, D45 shape) and report each
    task's task_id and dependsOn, in file order — replacing the enumerate + state-load agents'
    mechanical transcription with a direct parse. Returns [] if the file is missing, not valid
    JSON, or not a non-empty array — the caller (task 4 / the engine) decides whether that is a
    D16 refusal condition; this function only reports what is on disk."""
    tasks_path = os.path.join(repo_root, 'planning', spec_slug, 'tasks.json')
    if not os.path.exists(tasks_path):
        return []
    try:
        with open(tasks_path) as f:
            data = json.load(f)
    except (OSError, ValueError):
        return []
    if not isinstance(data, list):
        return []
    enumeration = []
    for task in data:
        if not isinstance(task, dict) or 'task_id' not in task:
            continue
        enumeration.append({
            'task_id': task.get('task_id'),
            'dependsOn': task.get('dependsOn', []),
        })
    return enumeration


def run_lint(repo_root, spec_slug):
    """Fold check_tasks_json.py's own verdict into prepare_run.py's output. Reimplements its
    main() loop field-for-field (same registry, same resolve_enabled(), same per-finding
    rule_id/fix_hint/message triple) via direct import rather than a subprocess, so this MUST
    equal `check_tasks_json.py <tasks.json>` run standalone on the same input (task 4 AC4). A
    missing spec_slug or tasks.json is not a lint failure — there is nothing to lint yet."""
    if not spec_slug:
        return {'passed': True, 'findings': [], 'enabled_rules': 0, 'total_rules': len(lint_rules.REGISTRY)}
    tasks_json_path = Path(repo_root) / 'planning' / spec_slug / 'tasks.json'
    if not tasks_json_path.exists():
        return {'passed': True, 'findings': [], 'enabled_rules': 0, 'total_rules': len(lint_rules.REGISTRY)}

    harness_path = Path(repo_root) / 'planning' / 'harness.json'
    harness_config = check_tasks_json.load_harness_config(harness_path)
    lint_rules_config = harness_config.get('lintRules') or {}
    enabled_rules = check_tasks_json.resolve_enabled(lint_rules.REGISTRY, lint_rules_config)

    findings = []
    for rule in enabled_rules:
        for f in rule['check'](tasks_json_path, harness_config):
            message = f.get('message', str(f)) if isinstance(f, dict) else str(f)
            findings.append({'rule_id': rule['id'], 'fix_hint': rule['fix_hint'], 'message': message})

    return {
        'passed': len(findings) == 0,
        'findings': findings,
        'enabled_rules': len(enabled_rules),
        'total_rules': len(lint_rules.REGISTRY),
    }


def _service_reachable(service):
    """Best-effort `requires.services` check. A `host:port` shaped name gets a short TCP connect
    probe; anything else has no defined protocol to probe, so it is treated as unverifiable and
    never refuses on it (a named check with no way to confirm it is not the same as a check that
    failed)."""
    host, sep, port_str = service.rpartition(':')
    if not sep:
        return True
    try:
        port = int(port_str)
    except ValueError:
        return True
    try:
        with socket.create_connection((host, port), timeout=1):
            return True
    except OSError:
        return False


def _missing_requirement(requires):
    """Return (kind, name) for the first unmet requirement in a `requires` object, or None if all
    are satisfied. Checked in bins -> env -> services order."""
    for name in requires.get('bins') or []:
        if shutil.which(name) is None:
            return ('bin', name)
    for name in requires.get('env') or []:
        if name not in os.environ:
            return ('env', name)
    for name in requires.get('services') or []:
        if not _service_reachable(name):
            return ('service', name)
    return None


def verify_requires_and_probes(repo_root, harness_config, simulate_missing_env=None):
    """Verify every harness.json validation.checks[] entry's `requires` is satisfied and every
    `probeCommand` runs clean, BEFORE any other prepare_run.py output is computed. Returns
    (reason, probes): `reason` is a refusal string (None if nothing refused); `probes` is the
    list of probe results recorded so far (only populated when nothing refused).

    --simulate-missing-env is checked first, ahead of any real harness.json entry, so a caller
    can exercise the refusal contract deterministically without needing a fixture check that
    declares `requires.env` (task 4 AC1 / the FE.7.B FELI_TEST_DATABASE_URL shape)."""
    for var in simulate_missing_env or []:
        if var not in os.environ:
            return (
                f"required environment variable '{var}' is not set (requires.env, simulated)",
                [],
            )

    checks = []
    if isinstance(harness_config, dict):
        checks = harness_config.get('validation', {}).get('checks', []) or []

    for check in checks:
        requires = check.get('requires') if isinstance(check, dict) else None
        if not requires:
            continue
        missing = _missing_requirement(requires)
        if missing:
            kind, name = missing
            return (
                f"check '{check.get('name')}' requires {kind} '{name}', which is not present",
                [],
            )

    probes = []
    for check in checks:
        probe_command = check.get('probeCommand') if isinstance(check, dict) else None
        if not probe_command:
            continue
        name = check.get('name')
        try:
            result = subprocess.run(
                probe_command, shell=True, cwd=repo_root,
                capture_output=True, text=True, timeout=30,
            )
        except subprocess.TimeoutExpired as exc:
            return (
                f"check '{name}''s probeCommand `{probe_command}` timed out: {exc}",
                [],
            )
        if result.returncode != 0:
            return (
                f"check '{name}''s probeCommand `{probe_command}` failed "
                f"(exit {result.returncode}): {result.stderr.strip()}",
                [],
            )
        probes.append({'check': name, 'command': probe_command, 'passed': True})

    return (None, probes)


def prepare_run(spec_slug, explicit_repo_root=None, cwd=None, simulate_missing_env=None):
    cwd = cwd or os.getcwd()
    repo_root = resolve_repo_root(explicit_repo_root)
    if not repo_root:
        return {
            'refused': True,
            'reason': 'could not resolve repo root via `git rev-parse --show-toplevel`',
        }

    harness_config = load_harness_config(repo_root)
    reason, probes = verify_requires_and_probes(repo_root, harness_config, simulate_missing_env)
    if reason:
        return {'refused': True, 'reason': reason}

    is_vaulted, vault_root = detect_vault(repo_root)
    agent_flag = render_agent_flag(cwd)
    scope_flag = render_scope_flag(cwd)
    tasks_enumeration = enumerate_tasks(repo_root, spec_slug) if spec_slug else []
    lint = run_lint(repo_root, spec_slug)
    return {
        'repo_root': repo_root,
        'is_vaulted': is_vaulted,
        'vault_root': vault_root,
        'agent_flag': agent_flag,
        'scope_flag': scope_flag,
        'harness_config': harness_config,
        'tasks_enumeration': tasks_enumeration,
        'lint': lint,
        'probes': probes,
        'refused': False,
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--spec-slug', default=None, help='spec slug under planning/<slug>/tasks.json')
    parser.add_argument('--repo-root', default=None, help='override repo root instead of resolving via git')
    parser.add_argument(
        '--simulate-missing-env', action='append', default=None, metavar='VAR',
        help='test-only: refuse if VAR is unset, as if a check declared requires.env: [VAR]',
    )
    args = parser.parse_args(argv)

    result = prepare_run(
        args.spec_slug,
        explicit_repo_root=args.repo_root,
        simulate_missing_env=args.simulate_missing_env,
    )
    print(json.dumps(result, indent=2))
    return 1 if result.get('refused') else 0


if __name__ == '__main__':
    sys.exit(main())
