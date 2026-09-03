#!/usr/bin/env python3
"""Validate block records against block.schema.json (D65).

Dependency-free on purpose: `jsonschema` is not installed anywhere in this fleet, so a
validator that imports it validates nothing and reports success. This checks the
constraints that actually matter — required fields, the ID grammar, the ID/filename/
spec_dir agreement, enums, date formats, and the dependency edge shapes — by hand.

It is the interim gate until mev's W_BLOCK_* checks ship
(MV.ticket.block-record-validation), and it stays useful afterwards as base-template's
own local check.

The planning/-path rule (BT.ticket.spec-files-under-planning-cannot-compile-in-ci) flags a
files[] path under planning/ as an ERROR, because planning/ is a symlink into the private HQ
vault (base-template/.gitignore:20) and code referencing such a path -- include_str!, a
fixture path, a test data file -- compiles on every developer machine and on no CI runner:
CI checkout cannot see the private vault. It is DELIBERATELY narrowed to build-input paths
only. A first, unnarrowed version flagged every files[] path under planning/ and produced 31
failures across this repo's 74 live block records, none of which are the failure mode this
rule is about -- every one was an AUTHORED PLANNING ARTIFACT (an ADR, harness.json,
status.md, a tasks.json) that is edited and never compiled, not a build input. So the
following are excluded, as authored planning artifacts rather than build inputs:
  - any `*.md` anywhere under planning/ (ADRs, status, notes, reports);
  - `planning/harness.json` and `planning/state.json` (config the harness reads at gate
    time, not a build input);
  - any `tasks.json` (a spec's task list, read by the engines, not compiled);
  - anything under a `planning/*/sdlc/` directory (engine run state).
Everything else under planning/ stays an ERROR.

Usage:
    check_block_records.py [--planning DIR] [--fleet] [--quiet]

    --planning DIR   validate one repo's planning/blocks/ (default: planning)
    --fleet          walk every _planning/<repo>/blocks/ under the brain root, plus the
                     brain root's own planning/blocks/
    --quiet          print only failures and the summary

Exit code 1 if any record fails. A repo with no blocks/ directory is not a failure —
that is the majority state during the D65 backfill and must stay silent.
"""

import argparse
import json
import os
import re
import sys

ID_RE = re.compile(r"^[A-Z]{2,4}\.(?:\d+[A-Z]?|ticket|chore)\.[A-Za-z0-9][A-Za-z0-9._-]*$")
DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")
COMMIT_RE = re.compile(r"^[0-9a-f]{7,40}$")
DIGEST_RE = re.compile(r"^[a-z0-9]+:[0-9a-f]+$")
# An operator/approval edge is cited in prose as `OP.<slug>` (D76, docs/state/state-schema.md
# ~line 89), so the slug is authored BARE kebab-case. A redundant `operator-` prefix stutters into
# `OP.operator-foo` and mev raises W_STATE_OP_SLUG_STUTTER on it (fix: `mev normalize-op-slugs
# --write`). This checker used to REQUIRE the prefix, i.e. it enforced the stutter -- measured
# 2026-09-01, 9 of the fleet's 13 operator/approval slugs carry it, which is why the severity here
# mirrors mev's exactly: a WARNING, never an error. Do not confuse this with lane.schema.json's
# `held_until`, which DOES take an `operator-`-prefixed token -- different field, different
# convention, both correct.
OPERATOR_SLUG_RE = re.compile(r"^[a-z0-9][a-z0-9-]*$")
OPERATOR_STUTTER_RE = re.compile(r"^operator-")

REQUIRED = ["id", "repo", "kind", "title", "description", "what", "why",
            "sdlc_workflow", "model", "out_of_scope",
            "acceptance_criteria", "spec_dir", "created", "updated"]

# `files` is load-bearing -- it is how /generate-tasks derives disjoint task ownership
# without guessing -- but it is a WARNING, not an error. Many blocks predate D65 and their
# master-plan sections never named paths; hard-requiring it during the backfill would force
# an agent to either invent file paths or refuse to write the record at all, and inventing
# is the precise failure D65 exists to end. A warning surfaces the debt without buying it
# back with fabrication. Same reasoning as mev's W_BLOCK_* codes shipping warning-first.
WARN_IF_MISSING = ["files", "validation_commands"]
KINDS = {"block", "ticket", "chore"}


def is_planning_authoring_artifact(fpath):
    """True when `fpath` (a files[] path already known to be under planning/) is an
    AUTHORED PLANNING ARTIFACT -- edited and never compiled -- rather than a build input.

    Scoped exactly to the class named in the planning/-path rule's docstring: any `*.md`
    under planning/, `planning/harness.json`, `planning/state.json`, any `tasks.json`, and
    anything under a `planning/*/sdlc/` directory. Everything else under planning/ is
    assumed to be a build input (a fixture, a test data file, a path a program reads at
    compile/run time) and stays flagged.
    """
    norm = fpath.replace(os.sep, "/")
    if norm.endswith(".md"):
        return True
    if norm in ("planning/harness.json", "planning/state.json"):
        return True
    if os.path.basename(norm) == "tasks.json":
        return True
    parts = norm.split("/")
    if "planning" in parts and "sdlc" in parts:
        pi = parts.index("planning")
        si = parts.index("sdlc")
        if si > pi:
            return True
    return False

WORKFLOWS = {"none", "patch", "task", "run", "flow"}
MODELS = {"sonnet", "gemini-pro", "gemini-flash", "either"}


# --- brain.toml prefix resolution ---------------------------------------------------------
# A block ID's prefix declares which repo's NAMESPACE it lives in; the `repo` field declares which
# repo OWNS the work. When they disagree, the block is filed into another repo's namespace and
# nothing downstream notices: check_block_naming.py reads the same [[repos]] prefixes but validates
# SPEC DIRECTORY NAMES, so a record's repo field is out of its scope by construction. Measured
# 2026-09-01 on the context-handling-between-nodes run: `sequence.md` filed the unattended-migration
# -runner block as `EN.14.H` with repo `agentic-portfolio` ("filed under HQ because the files are
# HQ's"). EN is engine-rs's prefix. An agent copying that row registers it into HQ's graph under
# engine-rs's namespace.

_PREFIX_CACHE = {}


def load_repo_prefixes(start):
    """prefix -> repo slug, from brain.toml's [[repos]] table, walking up from `start`.

    Returns {} when no brain.toml is reachable or it cannot be read -- a standalone repo
    scaffolded from this template has no brain, and the check simply does not apply there.
    """
    current = os.path.abspath(start)
    if os.path.isfile(current):
        current = os.path.dirname(current)
    root = None
    while True:
        if os.path.exists(os.path.join(current, "brain.toml")):
            root = current
            break
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent
    if root is None:
        return {}
    if root in _PREFIX_CACHE:
        return _PREFIX_CACHE[root]
    out = {}
    try:
        import tomllib
        with open(os.path.join(root, "brain.toml"), "rb") as fh:
            data = tomllib.load(fh)
        for entry in data.get("repos", []):
            prefix, slug = entry.get("prefix"), entry.get("slug")
            if prefix and slug:
                out[prefix] = slug
    except Exception:                              # noqa: BLE001 - report nothing, never raise
        out = {}
    _PREFIX_CACHE[root] = out
    return out


def check(path, planning_root="planning"):
    """Return (errors, warnings) for one record file."""
    problems = []
    warnings = []

    def bad(msg):
        problems.append(msg)

    def warn(msg):
        warnings.append(msg)

    try:
        with open(path) as fh:
            b = json.load(fh)
    except Exception as exc:                      # noqa: BLE001 - report, never raise
        return [f"does not parse: {exc}"], []

    if not isinstance(b, dict):
        return ["top level must be an object"], []

    for field in REQUIRED:
        v = b.get(field)
        if v is None or (isinstance(v, (str, list, dict)) and len(v) == 0):
            bad(f"required field `{field}` is missing or empty")

    for field in WARN_IF_MISSING:
        v = b.get(field)
        if v is None or (isinstance(v, (str, list, dict)) and len(v) == 0):
            warn(f"`{field}` is empty — backfill debt, not a blocker")

    bid = b.get("id", "")
    if bid and not ID_RE.match(bid):
        bad(f"id `{bid}` does not match <PFX>.<phase|ticket|chore>.<name>")

    prefixes = load_repo_prefixes(planning_root)
    if bid and prefixes:
        pfx = bid.split(".")[0]
        owner = prefixes.get(pfx)
        if owner is None:
            warn(f"id prefix `{pfx}` is not registered in brain.toml's [[repos]] table")
        elif b.get("repo") and b["repo"] != owner:
            bad(f"id prefix `{pfx}` is {owner}'s namespace but `repo` is `{b['repo']}` — a block "
                f"filed under another repo's prefix registers into that repo's namespace. Renumber "
                f"it under {b['repo']}'s own prefix, or correct `repo` to `{owner}`")

    stem = os.path.splitext(os.path.basename(path))[0]
    if bid and stem != bid:
        bad(f"filename stem `{stem}` != id `{bid}`")

    # spec_dir: the question that matters is "does this point at a real spec", not "is the
    # directory name canonical". `/generate-tasks` step 2 is explicit that LEGACY directories
    # (`<phase>.<block>-<title>`, `chore-<slug>`, ...) still resolve and do not require migrating,
    # and this checker used to contradict that by hard-failing every one of them. It cost a real
    # run: HQ.9.A is a CLOSED block whose spec is `planning/chore-fleet-parking-pass/plan.md`, and
    # renaming that directory to satisfy the checker would have broken nine live citations across
    # two repos — including two that cite `plan.md:146` by line as the operator approval for 59
    # promoted rows. Rewriting closed history to make a checker green is the wrong trade.
    #
    # So: a spec_dir that does not exist on disk is a real defect (a pointer to nothing) and stays
    # an ERROR. A spec_dir that exists but is not the canonical `planning/<id>/` is a WARNING —
    # backfill debt, the same class as an empty `files`.
    spec = b.get("spec_dir", "")
    if bid and spec:
        canonical = f"planning/{bid}/"
        spec_abs = os.path.join(planning_root, os.path.relpath(spec, "planning")) \
            if spec.startswith("planning/") else os.path.join(planning_root, spec)
        if spec != canonical:
            if os.path.isdir(spec_abs):
                warn(f"spec_dir `{spec}` is legacy-named, not `{canonical}` — resolves, so not a "
                     f"blocker; migrate only if the block is still open")
            else:
                bad(f"spec_dir `{spec}` does not exist and is not `{canonical}`")
        elif not os.path.isdir(spec_abs):
            # Canonical but absent is the normal state of a block whose spec has not been
            # generated yet — `/generate-tasks` creates the directory. Only a NON-canonical
            # path that also does not resolve is a genuine dangling pointer, handled above.
            warn(f"spec_dir `{spec}` does not exist yet — run /generate-tasks {bid}")

    if b.get("kind") not in KINDS:
        bad(f"kind `{b.get('kind')}` not one of {sorted(KINDS)}")
    if b.get("sdlc_workflow") not in WORKFLOWS:
        bad(f"sdlc_workflow `{b.get('sdlc_workflow')}` not one of {sorted(WORKFLOWS)}")
    if b.get("model") not in MODELS:
        bad(f"model `{b.get('model')}` not one of {sorted(MODELS)}")

    if b.get("kind") == "block" and b.get("phase") is None:
        bad("kind `block` requires `phase`")
    if b.get("kind") == "ticket" and not b.get("testing_strategy"):
        bad("kind `ticket` requires a non-empty `testing_strategy`")

    for field in ("created", "updated", "closed"):
        v = b.get(field)
        if v is not None and not DATE_RE.match(str(v)):
            bad(f"{field} `{v}` is not YYYY-MM-DD")
    if b.get("commit") is not None and not COMMIT_RE.match(str(b["commit"])):
        bad(f"commit `{b['commit']}` is not a hex git hash")

    files = b.get("files")
    if isinstance(files, dict):
        if not files.get("new") and not files.get("modified"):
            warn("files names neither a new nor a modified path")
        for key, req in (("new", "purpose"), ("modified", "change")):
            for i, f in enumerate(files.get(key) or []):
                if not isinstance(f, dict) or not f.get("path") or not f.get(req):
                    bad(f"files.{key}[{i}] needs both `path` and `{req}`")
                fpath = isinstance(f, dict) and f.get("path")
                if isinstance(fpath, str) and (
                        fpath == "planning" or fpath.startswith("planning/")) and \
                        not is_planning_authoring_artifact(fpath):
                    # `planning/` is a symlink into the private HQ vault, excluded from this
                    # repo's git by base-template/.gitignore:20 (the bare rule `/planning`).
                    # Code referencing such a path -- include_str!, a fixture path, a test
                    # data file -- compiles on every developer machine and on NO CI runner:
                    # local gates all pass and the build fails only in CI, where nothing
                    # reachable from the developer's machine could have caught it. Put the
                    # fixture or test data under tests/ instead.
                    bad(f"files.{key}[{i}] path `{fpath}` is under planning/ -- planning/ is a "
                        f"symlink into the private HQ vault, excluded from this repo's git by "
                        f"base-template/.gitignore:20 (`/planning`). Code referencing this path "
                        f"compiles on every developer machine and on no CI runner -- CI checkout "
                        f"cannot see the private vault, so every local gate passes and the build "
                        f"fails only in CI. Put the fixture or test data under tests/ instead")
    elif files is not None:
        bad("files must be an object with `new` / `modified`")

    for i, c in enumerate(b.get("acceptance_criteria") or []):
        if isinstance(c, str):
            continue
        if not isinstance(c, dict) or not c.get("criterion"):
            bad(f"acceptance_criteria[{i}] must be a string or carry `criterion`")
        elif c.get("gateable") is False and not c.get("evidence"):
            # D64: an un-gateable criterion with no fixture is the failure the rule exists
            # to catch -- it reads as verified while nothing observes it.
            bad(f"acceptance_criteria[{i}] is gateable:false but names no `evidence`")

    for i, e in enumerate(b.get("depends_on") or []):
        if not isinstance(e, dict):
            bad(f"depends_on[{i}] must be an object")
            continue
        t = e.get("type")
        if t == "block":
            if not e.get("repo") or not e.get("id"):
                bad(f"depends_on[{i}] block edge needs `repo` and `id`")
        elif t == "external":
            if not e.get("what"):
                bad(f"depends_on[{i}] external edge needs `what`")
        elif t == "operator":
            for k in ("slug", "exit", "start"):
                if not e.get(k):
                    bad(f"depends_on[{i}] operator edge needs `{k}`")
            slug = e.get("slug")
            if slug and OPERATOR_STUTTER_RE.match(slug):
                warn(f"depends_on[{i}] operator slug `{slug}` carries a redundant `operator-` "
                     f"prefix — it is cited as OP.{slug}, which stutters "
                     f"(W_STATE_OP_SLUG_STUTTER). Fix with `mev normalize-op-slugs --write`")
            elif slug and not OPERATOR_SLUG_RE.match(slug):
                bad(f"depends_on[{i}] operator slug `{slug}` must be bare kebab-case "
                    f"(no `operator-` prefix — it is rendered as OP.<slug>)")
        elif t == "approval":
            for k in ("slug", "what", "digest"):
                if not e.get(k):
                    bad(f"depends_on[{i}] approval edge needs `{k}`")
            if e.get("digest") and not DIGEST_RE.match(e["digest"]):
                bad(f"depends_on[{i}] digest `{e['digest']}` must be <algo>:<hex>")
            # An approval edge is cited as OP.<slug> too, so it stutters identically.
            if e.get("slug") and OPERATOR_STUTTER_RE.match(e["slug"]):
                warn(f"depends_on[{i}] approval slug `{e['slug']}` carries a redundant "
                     f"`operator-` prefix — cited as OP.{e['slug']}, which stutters "
                     f"(W_STATE_OP_SLUG_STUTTER). Fix with `mev normalize-op-slugs --write`")
        else:
            bad(f"depends_on[{i}] unknown type `{t}`")

    return problems, warnings


def blocks_dirs(fleet, planning):
    if not fleet:
        return [os.path.join(planning, "blocks")]
    root = os.getcwd()
    found = []
    for dirpath, dirnames, _ in os.walk(root, followlinks=False):
        dirnames[:] = [d for d in dirnames
                       if d not in {"node_modules", ".git", "archive", "target"}]
        if os.path.basename(dirpath) == "blocks" and (
                "_planning" in dirpath.split(os.sep)
                or dirpath.endswith(os.path.join("planning", "blocks"))):
            found.append(dirpath)
    return sorted(set(found))


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--planning", default="planning")
    ap.add_argument("--fleet", action="store_true")
    ap.add_argument("--quiet", action="store_true")
    args = ap.parse_args()

    total = failed = warned = 0
    for d in blocks_dirs(args.fleet, args.planning):
        if not os.path.isdir(d):
            continue
        for name in sorted(os.listdir(d)):
            if not name.endswith(".json"):
                continue
            path = os.path.join(d, name)
            total += 1
            # `d` is the blocks/ dir; the repo's planning root is its parent. spec_dir values are
            # written repo-relative as `planning/<...>/`, so resolving them needs that root, not cwd.
            problems, warnings = check(path, planning_root=os.path.dirname(d))
            if problems:
                failed += 1
                print(f"FAIL {path}")
                for p in problems:
                    print(f"       {p}")
            elif warnings:
                warned += 1
                if not args.quiet:
                    print(f"warn {path}")
            elif not args.quiet:
                print(f"ok   {path}")
            for w in warnings:
                if problems or not args.quiet:
                    print(f"       (warn) {w}")

    if total == 0:
        print("no block records found (not a failure)")
        return 0
    print(f"\n{total} record(s) checked, {failed} failed, {warned} with warnings")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
