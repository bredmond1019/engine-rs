#!/usr/bin/env python3
"""gate_cache.py — the lazy cross-lane gate cache keyed (repo, base_sha, lockfile hash).

BT.ticket.failure-attribution-and-gate-cache, task 2: a standalone Python 3 module + CLI that lets
the attribution decision (task 3) ask "was check T already red at base_sha" without re-running the
whole suite every time a lane happens to land on a base_sha another lane already gated. A HIT
(the triple has a recorded status for the requested check id) means re-run nothing. A MISS means
the caller must actually run that check id at base_sha and then `warm` the result in — this module
never runs a check itself; it only stores and serves what callers report.

Key shape: (repo, base_sha, lockfile_hash). `lockfile_hash` is computed from whatever dependency
lock files the target repo actually has on disk (Cargo.lock, package-lock.json, poetry.lock, ...).
A repo with none of those is not an error case — `compute_lockfile_hash()` returns the stable
sentinel `"no-lockfile"` so the triple still forms a valid, reproducible key.

Cache location resolves through an override chain (never a literal at a call site — standing
rule 12):
  1. `GATE_CACHE_DIR` env var, if set and non-empty.
  2. `planning/harness.json`'s `gateCache.dir` config value (relative to repo_root), if present.
  3. The documented default: `<repo_root>/.claude/workflows/.gate-cache/`.

On-disk shape: one JSON file per (repo, base_sha, lockfile_hash) triple, named
`<cache_dir>/<repo>/<base_sha>__<lockfile_hash>.json`, holding:
  {"repo": ..., "base_sha": ..., "lockfile_hash": ..., "warmed_at": <unix ts>,
   "results": {"<check_id>": "<status>", ...}}

Any read of a corrupt, truncated, or unreadable cache file degrades to a MISS for every requested
check id — an attribution look-back must never be blocked by cache state (task 2 AC4).

Usage:
  python3 .claude/workflows/bin/gate_cache.py lookup --repo <repo> --base-sha <sha> \\
      --check-ids <id1,id2,...> [--repo-root <path>] [--lockfile-hash <hash>]

  python3 .claude/workflows/bin/gate_cache.py warm --repo <repo> --base-sha <sha> \\
      --result <check_id>=<status> [--result <check_id>=<status> ...] \\
      [--repo-root <path>] [--lockfile-hash <hash>]

  python3 .claude/workflows/bin/gate_cache.py status [--repo <repo>] [--base-sha <sha>] \\
      [--repo-root <path>]

Each verb prints one JSON object to stdout and exits 0. `lookup` and `warm` never raise on a
missing/corrupt cache file — they report the degrade instead.
"""

import argparse
import hashlib
import json
import os
import sys
import time

# Dependency lock files this module knows to fingerprint, in a fixed, sorted order so the hash is
# reproducible regardless of directory-listing order. Extend this list as new stacks are added —
# it is data, not a decision point that needs its own config knob (there is no "wrong" lockfile to
# include; a repo simply doesn't have the ones that don't apply to it).
_KNOWN_LOCKFILES = (
    'Cargo.lock',
    'package-lock.json',
    'pnpm-lock.yaml',
    'yarn.lock',
    'poetry.lock',
    'requirements.txt',
    'Pipfile.lock',
    'Gemfile.lock',
    'go.sum',
    'composer.lock',
)

_NO_LOCKFILE_SENTINEL = 'no-lockfile'

_DEFAULT_CACHE_SUBDIR = os.path.join('.claude', 'workflows', '.gate-cache')


def compute_lockfile_hash(repo_root):
    """Fingerprint whatever dependency lock files `repo_root` actually has. Returns the stable
    sentinel `_NO_LOCKFILE_SENTINEL` when none of the known lockfiles are present — that absence
    is itself a defined, stable key component, not an error (task 2 requirement)."""
    digest = hashlib.sha256()
    found_any = False
    for name in _KNOWN_LOCKFILES:
        path = os.path.join(repo_root, name)
        try:
            with open(path, 'rb') as f:
                content = f.read()
        except OSError:
            continue
        found_any = True
        digest.update(name.encode('utf-8'))
        digest.update(b'\0')
        digest.update(content)
        digest.update(b'\0')
    if not found_any:
        return _NO_LOCKFILE_SENTINEL
    return digest.hexdigest()


def _load_harness_config(repo_root):
    harness_path = os.path.join(repo_root, 'planning', 'harness.json')
    try:
        with open(harness_path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def resolve_cache_dir(repo_root, harness_config=None, env=None):
    """Override chain: GATE_CACHE_DIR env var -> harness.json `gateCache.dir` -> the documented
    default under `.claude/workflows/.gate-cache/`. Never hardcode this at a call site."""
    env = env if env is not None else os.environ

    env_override = (env.get('GATE_CACHE_DIR') or '').strip()
    if env_override:
        return env_override if os.path.isabs(env_override) else os.path.join(repo_root, env_override)

    if harness_config is None:
        harness_config = _load_harness_config(repo_root)
    if isinstance(harness_config, dict):
        configured = ((harness_config.get('gateCache') or {}).get('dir') or '').strip() \
            if isinstance(harness_config.get('gateCache'), dict) else ''
        if configured:
            return configured if os.path.isabs(configured) else os.path.join(repo_root, configured)

    return os.path.join(repo_root, _DEFAULT_CACHE_SUBDIR)


def _entry_path(cache_dir, repo, base_sha, lockfile_hash):
    # Keep the on-disk name filesystem-safe; repo/base_sha/lockfile_hash are already
    # slug/hex-shaped in every real caller, but guard against a stray path separator anyway.
    safe_repo = repo.replace(os.sep, '_')
    return os.path.join(cache_dir, safe_repo, f'{base_sha}__{lockfile_hash}.json')


def _read_entry(cache_dir, repo, base_sha, lockfile_hash):
    """Fail soft: any missing file, unreadable file, or malformed JSON is treated as no entry
    (a miss for every check id), never an exception (task 2 AC4)."""
    path = _entry_path(cache_dir, repo, base_sha, lockfile_hash)
    try:
        with open(path) as f:
            data = json.load(f)
    except (OSError, ValueError):
        return None
    if not isinstance(data, dict) or not isinstance(data.get('results'), dict):
        return None
    return data


def _write_entry(cache_dir, repo, base_sha, lockfile_hash, results, warmed_at=None):
    path = _entry_path(cache_dir, repo, base_sha, lockfile_hash)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    payload = {
        'repo': repo,
        'base_sha': base_sha,
        'lockfile_hash': lockfile_hash,
        'warmed_at': warmed_at if warmed_at is not None else time.time(),
        'results': results,
    }
    tmp_path = path + '.tmp'
    with open(tmp_path, 'w') as f:
        json.dump(payload, f, indent=2, sort_keys=True)
        f.write('\n')
    os.replace(tmp_path, path)
    return payload


def lookup(cache_dir, repo, base_sha, lockfile_hash, check_ids):
    """A HIT is a check id present in the recorded entry's `results` — its recorded status is
    returned and the caller re-runs nothing for it. Every other requested id is a MISS: the caller
    must actually run it at base_sha (never the whole suite) and then `warm` the result in."""
    entry = _read_entry(cache_dir, repo, base_sha, lockfile_hash)
    recorded = entry.get('results', {}) if entry else {}
    hits = {cid: recorded[cid] for cid in check_ids if cid in recorded}
    misses = [cid for cid in check_ids if cid not in recorded]
    return {
        'repo': repo,
        'base_sha': base_sha,
        'lockfile_hash': lockfile_hash,
        'hits': hits,
        'misses': misses,
    }


def warm(cache_dir, repo, base_sha, lockfile_hash, results):
    """Merge freshly-run `results` (check_id -> status) into the existing entry for this triple,
    if any, and write it back. Manual verb only (block out_of_scope: no scheduled warm yet) — a
    caller invokes this after it has itself run the check ids a `lookup` reported as misses."""
    existing = _read_entry(cache_dir, repo, base_sha, lockfile_hash)
    merged = dict(existing.get('results', {})) if existing else {}
    merged.update(results)
    return _write_entry(cache_dir, repo, base_sha, lockfile_hash, merged)


def status(cache_dir, repo=None, base_sha=None):
    """List recorded entries, optionally filtered by repo and/or base_sha. Corrupt entries are
    skipped rather than raising, consistent with lookup()'s fail-soft contract."""
    entries = []
    if not os.path.isdir(cache_dir):
        return entries
    for repo_name in sorted(os.listdir(cache_dir)):
        if repo is not None and repo_name != repo.replace(os.sep, '_'):
            continue
        repo_dir = os.path.join(cache_dir, repo_name)
        if not os.path.isdir(repo_dir):
            continue
        for fname in sorted(os.listdir(repo_dir)):
            if not fname.endswith('.json'):
                continue
            path = os.path.join(repo_dir, fname)
            try:
                with open(path) as f:
                    data = json.load(f)
            except (OSError, ValueError):
                continue
            if not isinstance(data, dict):
                continue
            if base_sha is not None and data.get('base_sha') != base_sha:
                continue
            entries.append(data)
    return entries


def _parse_result_pairs(pairs):
    results = {}
    for pair in pairs or []:
        if '=' not in pair:
            raise ValueError(f"--result must be <check_id>=<status>, got: {pair!r}")
        check_id, _, value = pair.partition('=')
        results[check_id] = value
    return results


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--repo-root', default=None, help='repo root; defaults to the current directory')
    sub = parser.add_subparsers(dest='verb', required=True)

    p_lookup = sub.add_parser('lookup', help='report hit/miss per requested check id at (repo, base_sha)')
    p_lookup.add_argument('--repo', required=True)
    p_lookup.add_argument('--base-sha', required=True)
    p_lookup.add_argument('--check-ids', required=True, help='comma-separated check ids')
    p_lookup.add_argument('--lockfile-hash', default=None, help='override instead of computing from repo-root')

    p_warm = sub.add_parser('warm', help='record freshly-run results for (repo, base_sha)')
    p_warm.add_argument('--repo', required=True)
    p_warm.add_argument('--base-sha', required=True)
    p_warm.add_argument('--result', action='append', default=[], help='<check_id>=<status>, repeatable')
    p_warm.add_argument('--lockfile-hash', default=None, help='override instead of computing from repo-root')

    p_status = sub.add_parser('status', help='list recorded cache entries')
    p_status.add_argument('--repo', default=None)
    p_status.add_argument('--base-sha', default=None)

    args = parser.parse_args(argv)
    repo_root = os.path.abspath(args.repo_root) if args.repo_root else os.getcwd()
    cache_dir = resolve_cache_dir(repo_root)

    if args.verb == 'lookup':
        lockfile_hash = args.lockfile_hash or compute_lockfile_hash(repo_root)
        check_ids = [c for c in args.check_ids.split(',') if c]
        result = lookup(cache_dir, args.repo, args.base_sha, lockfile_hash, check_ids)
    elif args.verb == 'warm':
        lockfile_hash = args.lockfile_hash or compute_lockfile_hash(repo_root)
        try:
            results = _parse_result_pairs(args.result)
        except ValueError as exc:
            print(json.dumps({'error': str(exc)}, indent=2))
            return 1
        result = warm(cache_dir, args.repo, args.base_sha, lockfile_hash, results)
    else:  # status
        result = {'entries': status(cache_dir, repo=args.repo, base_sha=args.base_sha)}

    print(json.dumps(result, indent=2, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main())
